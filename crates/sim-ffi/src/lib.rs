//! # sim-ffi
//!
//! C ABI exports consumed by the FreeRTOS port layer.
//!
//! This crate provides `#[no_mangle]` functions that the C port layer
//! (port.c, sim_hooks.c) calls to interact with the Rust simulator.
//!
//! # Architecture
//!
//! All heavy state lives behind a single-threaded `RefCell`.  The
//! scheduler never holds a borrow across fiber resume (which would
//! cause `RefCell` panics on re-entrant calls from within a fiber).
//! Yields from C code go through the TLS yielder directly and do not
//! touch the global state.
//!
//! Functions that CAN be called from within a running fiber (and thus
//! must not panic when the global RefCell is borrowed) use separate
//! thread-local storage or atomic primitives:
//!   - `sim_port_yield` → TLS yielder (never touches global)
//!   - `sim_task_exit` → TLS yielder (never touches global)
//!   - `sim_now_ticks` → atomic Tick (lock-free read)
//!   - `sim_enter_critical` / `sim_exit_critical` → separate TLS counter
//!   - `sim_trace_u32` → append to a thread-local trace buffer

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};

use sim_core::time::Tick;
use sim_core::trace::{TraceEvent, TraceSink};
use sim_fiber::yield_reason::YieldReason;
use sim_fiber::{suspend_active_fiber, Fiber, TaskId};

pub mod device_ffi;
pub mod guest_runtime;
pub mod net_ffi;
pub mod simulator;
pub mod zephyr_ffi;

use device_ffi::deliver_pending_irqs;
use net_ffi::{eth_loopback_bridge, tap_eth_bridge};

// ── C functions called FROM Rust (implemented in task.c) ──────────

#[link(name = "embedded_c_payload", kind = "static")]
extern "C" {
    fn sim_set_current_task_by_id(task_id: u64);
    /// Single-tick advance. C code calls this via sim_advance_ticks(1)
    /// rather than directly, so Rust never invokes it. The extern
    /// declaration is kept for completeness and as documentation of
    /// the C ABI surface.
    #[allow(dead_code)]
    fn sim_tick_advance() -> u32;
    fn sim_advance_ticks(count: u32) -> u32;
    fn sim_bridge_create_pending_fibers() -> u32;
}

// ---------------------------------------------------------------------------
// Thread-local state (re-entrant safe)
// ---------------------------------------------------------------------------

/// Current virtual time.  Atomic so it can be read from within a fiber
/// without touching the global RefCell.
pub(crate) static SIM_NOW: AtomicU64 = AtomicU64::new(0);

/// Current task ID of the executing fiber, if any.
/// Atomic so it can be read from within a fiber (e.g., by
/// `sim_host_block_on_fd`) without touching the global RefCell.
/// Set by the scheduler before resuming a fiber, cleared after.
pub(crate) static CURRENT_TASK_ID: AtomicU64 = AtomicU64::new(0);

thread_local! {
    pub(crate) static CRITICAL_NESTING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

thread_local! {
    pub(crate) static TL_TRACE: RefCell<Vec<sim_core::trace::TraceEvent>> =
        const { RefCell::new(Vec::new()) };
}

thread_local! {
    /// Pending task deletions recorded from within a fiber context.
    ///
    /// When `vTaskDelete` is called from C code running inside a fiber,
    /// the `traceTASK_DELETE` hook calls `sim_task_deleted`, which pushes
    /// the task ID here instead of directly touching `SIM_GLOBAL`
    /// (which is already borrowed by the scheduler).  After the fiber
    /// yields, `process_pending_deletions()` drains this list and marks
    /// the tasks as `Exited` in the global state.
    pub(crate) static PENDING_DELETIONS: RefCell<Vec<u64>> =
        const { RefCell::new(Vec::new()) };
}

// ── Budget state (function-entry instrumentation) ─────────────────

/// Per-task budget for detecting CPU-bound stalls.
///
/// When function-entry instrumentation is enabled (-finstrument-functions),
/// every C function entry calls `sim_budget_poll`, which increments a
/// counter.  If the counter exceeds the budget, the fiber yields with
/// `BudgetExceeded` so the scheduler can run other tasks.
pub struct BudgetState {
    /// Number of function entries since last reset.
    pub entry_count: u64,
    /// Maximum function entries allowed before forcing a yield.
    pub max_entries: u64,
    /// Whether the budget has been exceeded (task should yield).
    pub exceeded: bool,
}

impl BudgetState {
    pub const fn new() -> Self {
        Self {
            entry_count: 0,
            max_entries: 1_000_000,
            exceeded: false,
        }
    }
}

impl Default for BudgetState {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    pub(crate) static BUDGET: std::cell::RefCell<BudgetState> =
        const { std::cell::RefCell::new(BudgetState::new()) };
}

// ---------------------------------------------------------------------------
// Global state (accessed only from the scheduler outside fiber context)
// ---------------------------------------------------------------------------

/// Interrupt state for virtual critical sections.
#[derive(Debug, Clone, Copy, Default)]
pub struct InterruptState {
    /// Whether virtual interrupts are locked.
    pub locked: bool,
}

/// Global simulator state.
///
/// This is accessed only from the scheduler main loop, never from
/// within a running fiber.  Functions called from fibers use separate
/// TLS or atomic state.
pub struct SimGlobal {
    /// All simulated tasks (fibers).
    pub tasks: Vec<Fiber>,
    /// Index of the currently running task, if any.
    pub current_task: Option<usize>,
    /// Interrupt state for critical sections.
    pub interrupt: InterruptState,
    /// Next task ID.
    pub next_task_id: TaskId,
    /// Trace sink (may be null before initialization).
    pub trace: Option<Box<TraceSink>>,
}

impl SimGlobal {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            current_task: None,
            interrupt: InterruptState::default(),
            next_task_id: 1,
            trace: None,
        }
    }
}

impl Default for SimGlobal {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    static SIM_GLOBAL: RefCell<SimGlobal> = RefCell::new(SimGlobal::new());
}

// ---------------------------------------------------------------------------
// Per-Simulator state: pointer to the currently active SimGlobal
// ---------------------------------------------------------------------------

thread_local! {
    /// Pointer to the currently active simulator's `SimGlobal` RefCell.
    ///
    /// When a [`Simulator`](crate::simulator::Simulator) is running, it sets
    /// this to point at its own `SimGlobal` so that C ABI functions find the
    /// right task pool and trace sink.  When `None`, the thread-local
    /// `SIM_GLOBAL` fallback is used (for backward compatibility with tests
    /// that call `init_global` directly).
    pub(crate) static ACTIVE_SIM_GLOBAL: Cell<Option<*const RefCell<SimGlobal>>> =
        const { Cell::new(None) };
}

/// Access the current `SimGlobal` — either the active simulator's or the
/// thread-local fallback.
///
/// This is the single access point used by all C ABI functions; replacing
/// `SIM_GLOBAL.with(...)` with `with_sim_global(...)` is the only change
/// needed to support per-Simulator isolation.
#[inline]
pub(crate) fn with_sim_global<F, R>(f: F) -> R
where
    F: FnOnce(&RefCell<SimGlobal>) -> R,
{
    ACTIVE_SIM_GLOBAL.with(|active| {
        if let Some(ptr) = active.get() {
            // Safety: the pointer is valid because the Simulator that set it
            // is alive on the stack above us (activate/deactivate guard).
            let global_ref = unsafe { &*ptr };
            f(global_ref)
        } else {
            SIM_GLOBAL.with(|g| f(g))
        }
    })
}

/// Activate a `SimGlobal` for the current thread — C ABI calls will
/// operate on this state until deactivated.
///
/// # Safety
///
/// The caller must ensure the returned guard is dropped before `sim_global`
/// is dropped.  Typically this is done by calling `activate()` on a
/// `Simulator` before running and letting the guard drop afterward.
pub(crate) fn activate_sim_global(sim_global: &RefCell<SimGlobal>) -> SimGlobalGuard {
    let ptr: *const RefCell<SimGlobal> = sim_global;
    let old = ACTIVE_SIM_GLOBAL.with(|active| active.replace(Some(ptr)));
    SimGlobalGuard { old }
}

/// Opaque guard that restores the previous `SimGlobal` on drop.
pub(crate) struct SimGlobalGuard {
    old: Option<*const RefCell<SimGlobal>>,
}

impl Drop for SimGlobalGuard {
    fn drop(&mut self) {
        ACTIVE_SIM_GLOBAL.with(|active| active.set(self.old));
    }
}

// ---------------------------------------------------------------------------
// C ABI exports
// ---------------------------------------------------------------------------

/// Return the current virtual time in ticks.
///
/// # Safety
///
/// Always safe — uses an atomic relaxed read and never touches
/// the global RefCell.  Can be called from any context.
#[no_mangle]
pub unsafe extern "C" fn sim_now_ticks() -> u64 {
    SIM_NOW.load(Ordering::Relaxed)
}

/// Set the current virtual time (called from the scheduler only).
pub fn set_sim_now(now: Tick) {
    SIM_NOW.store(now, Ordering::Relaxed);
}

/// Register a new simulated task.
///
/// Returns an opaque handle, or 0 on failure.
///
/// # Safety
///
/// Must NOT be called from within a running fiber.  `name_ptr` must be
/// a valid null-terminated C string.  `entry` must be a valid function
/// pointer that follows the C ABI.  `arg` must be valid (or null) for
/// the entry function's parameter type.
///
/// This function borrows the global RefCell and will panic if called
/// re-entrantly from a borrow that is already held.
#[no_mangle]
pub unsafe extern "C" fn sim_create_task(
    name_ptr: *const std::ffi::c_char,
    entry: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    arg: *mut std::ffi::c_void,
    requested_stack_words: u32,
    priority: u32,
) -> usize {
    with_sim_global(|global| {
        let mut global = global.borrow_mut();

        let name = if name_ptr.is_null() {
            "unnamed"
        } else {
            let c_str = std::ffi::CStr::from_ptr(name_ptr);
            c_str.to_str().unwrap_or("unnamed")
        };
        let name_static: &'static str = Box::leak(name.to_string().into_boxed_str());

        let entry = entry.expect("sim_create_task: NULL entry point");

        let id = global.next_task_id;
        global.next_task_id += 1;

        let _pri = priority;

        let fiber = Fiber::new(
            id,
            name_static,
            priority,
            requested_stack_words,
            sim_fiber::MIN_HOST_COROUTINE_STACK,
            id,
            move |_reason| {
                // Safety: we're in a fiber, TLS is set.
                unsafe {
                    entry(arg);
                }
                // Signal task exit via TLS (doesn't touch global).
                suspend_active_fiber(YieldReason::TaskExit);
            },
        );
        global.tasks.push(fiber);

        // Emit a TaskCreated trace event so symbolication tools can
        // resolve task IDs to names.
        if let Some(ref mut trace) = global.trace {
            trace.record(TraceEvent::TaskCreated {
                at: SIM_NOW.load(Ordering::Relaxed),
                task: id,
                name: name_static,
            });
        }

        id as usize
    })
}

/// Register a human-readable symbol name for a task.
///
/// This can be called after `sim_create_task` to associate a name with
/// a task ID that was already created.  Useful for tasks created by the
/// RTOS kernel (e.g., idle tasks, timer daemon) that get their names
/// indirectly.
///
/// # Safety
///
/// `name_ptr` must be a valid null-terminated C string or null.  Must
/// NOT be called from within a running fiber.
#[no_mangle]
pub unsafe extern "C" fn sim_register_symbol(task_id: u64, name_ptr: *const std::ffi::c_char) {
    let name = if name_ptr.is_null() {
        "unnamed"
    } else {
        let c_str = std::ffi::CStr::from_ptr(name_ptr);
        c_str.to_str().unwrap_or("unnamed")
    };
    let name_static: &'static str = Box::leak(name.to_string().into_boxed_str());

    with_sim_global(|global| {
        let mut global = global.borrow_mut();
        if let Some(ref mut trace) = global.trace {
            trace.record(TraceEvent::TaskCreated {
                at: SIM_NOW.load(Ordering::Relaxed),
                task: task_id,
                name: name_static,
            });
        }
    });
}
// ---------------------------------------------------------------------------
// Scheduler tick state (persistent across sim_scheduler_tick() calls)
// ---------------------------------------------------------------------------

/// State persisted across tick-by-tick scheduler calls.
#[derive(Default)]
pub(crate) struct SchedulerTickState {
    /// Whether the one-time setup (exit_critical, create_pending_fibers)
    /// has been performed.
    pub(crate) initialized: bool,
    /// Current scheduler virtual time, carried forward across ticks.
    pub(crate) sim_time: Tick,
}

thread_local! {
    /// Per-thread scheduler tick state for `sim_scheduler_tick()`.
    /// Each thread that calls `sim_scheduler_tick()` gets its own
    /// independent state; `sim_start_scheduler()` does NOT use this.
    static SCHEDULER_TICK_STATE: RefCell<SchedulerTickState> =
        RefCell::new(SchedulerTickState::default());
}

thread_local! {
    /// Per-thread Zephyr scheduler tick state for `sim_zephyr_scheduler_tick()`.
    /// Separate from the FreeRTOS tick state so mixed-RTOS scenarios can
    /// advance Zephyr and FreeRTOS machines independently on the same thread.
    pub(crate) static ZEPHYR_SCHEDULER_TICK_STATE: RefCell<SchedulerTickState> =
        RefCell::new(SchedulerTickState::default());
}

// ---------------------------------------------------------------------------
// Scheduler cycle
// ---------------------------------------------------------------------------

/// Run one scheduler cycle.
///
/// Returns `true` if the simulation should continue (there are still
/// runnable or sleeping/I/O-waiting tasks), or `false` if the simulation
/// is complete (no runnable tasks and no sleeping tasks and no I/O progress).
///
/// `sim_time` is advanced in-place when virtual time progresses (e.g.,
/// during tickless idle fast-forward).
pub(crate) fn run_one_scheduler_cycle(sim_time: &mut Tick) -> bool {
    // ── Compute earliest sleeping task wake time ──────────────
    let next_wake: Option<Tick> = with_sim_global(|global| {
        let global = global.borrow();
        global
            .tasks
            .iter()
            .filter_map(|t| {
                if let sim_fiber::TaskState::Sleeping { until } = t.state {
                    Some(until)
                } else {
                    None
                }
            })
            .min()
    });

    // ── Try to find a runnable task (priority-ordered) ────
    let task_idx: Option<usize> = with_sim_global(|global| {
        let global = global.borrow();
        let task_count = global.tasks.len();

        if task_count == 0 {
            return None;
        }

        let mut runnable: Vec<usize> = (0..task_count)
            .filter(|&i| global.tasks[i].is_runnable())
            .collect();

        if runnable.is_empty() {
            return None;
        }

        // Sort by priority (higher priority first), then by
        // round-robin distance from the last scheduled task.
        let start = global.current_task.unwrap_or(0);
        runnable.sort_by(|&a, &b| {
            // Higher priority value = higher priority
            let pa = global.tasks[a].priority;
            let pb = global.tasks[b].priority;
            // Descending priority
            pb.cmp(&pa).then_with(|| {
                // Round-robin: prefer tasks closer to `start`
                let dist_a = (a + task_count - start) % task_count;
                let dist_b = (b + task_count - start) % task_count;
                dist_a.cmp(&dist_b)
            })
        });

        Some(runnable[0])
    });

    match task_idx {
        Some(idx) => {
            // ── Resume the selected task ──────────────────

            // Tell C which TCB is current.
            let task_id = with_sim_global(|global| {
                let mut global = global.borrow_mut();
                global.current_task = Some(idx);
                let tid = global.tasks[idx].id;
                if let Some(ref mut trace) = global.trace {
                    trace.record(sim_core::trace::TraceEvent::TaskResume {
                        at: *sim_time,
                        task: tid,
                        reason: "scheduler",
                    });
                }
                tid
            });

            // Safety: called outside fiber borrow window.
            unsafe {
                sim_set_current_task_by_id(task_id);
            }

            // Set the current task ID for re-entrant-safe access
            // from within the fiber (e.g., sim_host_block_on_fd).
            CURRENT_TASK_ID.store(task_id, Ordering::Relaxed);

            // Resume the fiber, catching panics so a single misbehaving
            // task does not crash the entire simulator process.
            let (yield_reason, panicked) = with_sim_global(|global| {
                let mut global = global.borrow_mut();
                let task = &mut global.tasks[idx];

                // Safety: resume() internally touches TLS and the
                // coroutine stack.  A panic inside a fiber must not
                // unwind across the corosensei stack-switch boundary
                // unchecked, but catch_unwind here means the panic
                // is contained and the task is marked Faulted.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    task.resume(sim_fiber::ResumeReason::SchedulerSelected)
                }));
                match result {
                    Ok(reason) => (reason, false),
                    Err(_panic_payload) => {
                        task.state = sim_fiber::TaskState::Faulted;
                        (Some(YieldReason::Fault), true)
                    }
                }
            });

            // Clear current task ID — the fiber is no longer active.
            CURRENT_TASK_ID.store(0, Ordering::Relaxed);

            // Handle yield.
            with_sim_global(|global| {
                let mut global = global.borrow_mut();
                if let Some(reason) = yield_reason {
                    if let Some(ref mut trace) = global.trace {
                        if panicked {
                            // Record the fatal panic event.
                            trace.record(sim_core::trace::TraceEvent::Fatal {
                                at: *sim_time,
                                code: sim_core::error::SimErrorCode::PanicCrossedCAbi,
                            });
                        }
                        let reason_str: &'static str =
                            Box::leak(format!("{:?}", reason).into_boxed_str());
                        trace.record(sim_core::trace::TraceEvent::TaskYield {
                            at: *sim_time,
                            task: task_id,
                            reason: reason_str,
                        });
                    }
                }

                // Flush TL trace into main trace.
                TL_TRACE.with(|tl| {
                    let mut tl = tl.borrow_mut();
                    if !tl.is_empty() {
                        if let Some(ref mut trace) = global.trace {
                            trace.events.append(&mut tl);
                        }
                        tl.clear();
                    }
                });
            });

            // Deliver any pending IRQs and expired timers.
            deliver_pending_irqs(*sim_time);

            // Process any task deletions recorded during the
            // fiber's execution (vTaskDelete from C code).
            process_pending_deletions();

            // Bridge Ethernet loopback: deliver guest-sent frames
            // back to the receive queue so receiver tasks can read them.
            eth_loopback_bridge();

            set_sim_now(*sim_time);

            true // work was done; continue
        }
        None => {
            // ── No runnable task ──────────────────────────

            // Process any pending task deletions first.
            process_pending_deletions();

            // Bridge Ethernet loopback in the idle path too.
            eth_loopback_bridge();

            //
            // Check the peripheral event queue alongside the
            // next RTOS wake time.  If a peripheral event is
            // sooner, advance to it and dispatch the callback
            // before processing RTOS timeouts.
            let event_deadline = next_event_deadline();

            match next_wake {
                Some(wake_time) if wake_time > *sim_time => {
                    // Tickless idle: batch-advance all the ticks
                    // in one C↔Rust crossing instead of one per tick.
                    //
                    // But first: if a peripheral event fires before
                    // the next RTOS wake, advance to the event first.
                    if let Some(ev) = event_deadline {
                        if ev < wake_time {
                            // Peripheral event before RTOS wake:
                            // advance to event, dispatch it, then
                            // fall through to handle RTOS wake.
                            *sim_time = ev;
                            set_sim_now(*sim_time);
                            dispatch_events(*sim_time);
                            deliver_pending_irqs(*sim_time);
                        }
                    }

                    // Advance ticks to the RTOS wake time.
                    let ticks_to_advance = (wake_time - *sim_time) as u32;
                    if ticks_to_advance > 0 {
                        *sim_time = wake_time;
                        unsafe {
                            sim_advance_ticks(ticks_to_advance);
                        }
                    }
                    // Deliver timer IRQs that may have fired during
                    // the advance.
                    deliver_pending_irqs(*sim_time);

                    // Wake fibers whose sleep time has passed.
                    with_sim_global(|global| {
                        let mut global = global.borrow_mut();
                        for task in global.tasks.iter_mut() {
                            task.try_wake(*sim_time);
                        }
                    });

                    // Deliver IRQs that may have been deferred.
                    deliver_pending_irqs(*sim_time);

                    // Also dispach any events at the new time.
                    dispatch_events(*sim_time);
                    deliver_pending_irqs(*sim_time);

                    // Also poll host FDs ...
                    let next_wake_after = with_sim_global(|global| {
                        let global = global.borrow();
                        global
                            .tasks
                            .iter()
                            .filter_map(|t| {
                                if let sim_fiber::TaskState::Sleeping { until } = t.state {
                                    Some(until)
                                } else {
                                    None
                                }
                            })
                            .min()
                    });
                    let io_waiting_after = with_sim_global(|global| {
                        let global = global.borrow();
                        global
                            .tasks
                            .iter()
                            .any(|t| matches!(t.state, sim_fiber::TaskState::IoWaiting))
                    });
                    if io_waiting_after {
                        host_poll_and_wake(*sim_time, next_wake_after);
                        deliver_pending_irqs(*sim_time);
                    }

                    set_sim_now(*sim_time);

                    true // time advanced; continue
                }
                _ => {
                    // ── No sleeping tasks — check for I/O-blocked tasks ─
                    let io_waiting = with_sim_global(|global| {
                        let global = global.borrow();
                        global
                            .tasks
                            .iter()
                            .any(|t| matches!(t.state, sim_fiber::TaskState::IoWaiting))
                    });

                    // Check whether a TAP bridge is active — if the host
                    // TAP interface is open, the simulation must stay alive
                    // to handle incoming frames from the host network.
                    #[cfg(unix)]
                    let tap_active =
                        sim_net::with_tap_bridge(|tap| tap.is_active()).unwrap_or(false);
                    #[cfg(not(unix))]
                    let tap_active = false;

                    if io_waiting || tap_active {
                        // Some tasks are blocked on host I/O (or TAP is
                        // active).  Poll host sockets and wake any whose
                        // FDs are ready.
                        let woken = host_poll_and_wake(*sim_time, next_wake);

                        // If TAP is active, process any incoming frames
                        // from the host (even if no tasks were woken).
                        if tap_active {
                            tap_eth_bridge();

                            // Check if TAP processing made any tasks
                            // runnable (e.g., a sleeping task was waiting
                            // for a network response).
                            let has_runnable = with_sim_global(|global| {
                                let global = global.borrow();
                                global.tasks.iter().any(|t| t.is_runnable())
                            });
                            if has_runnable {
                                set_sim_now(*sim_time);
                                return true; // continue
                            }
                        }

                        if woken > 0 {
                            // Tasks were woken — loop back to run them.
                            set_sim_now(*sim_time);
                            return true; // continue
                        }
                    }

                    // No sleeping tasks and no I/O progress — simulation complete.
                    false
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// C ABI: sim_start_scheduler (loop wrapper)
// ---------------------------------------------------------------------------

/// Start the scheduler — round-robin drain loop with virtual-time tick support.
///
/// The scheduler:
/// 1. Resumes runnable tasks round-robin.
/// 2. Sets pxCurrentTCB before resuming so the C kernel knows who is running.
/// 3. When no tasks are runnable, advances virtual time to the earliest
///    sleeping task's wake time, calling `sim_tick_advance()` for each tick
///    boundary crossed.
/// 4. Exits when no tasks are runnable and no tasks are sleeping.
///
/// # Safety
///
/// Must be called from the main scheduler context (not within a fiber).
/// Takes ownership of the calling thread and will not return until the
/// simulation completes.  Assumes the global trace sink has been
/// initialized via `init_global`.
///
/// This is a convenience wrapper that calls [`sim_scheduler_tick`] in a
/// loop until the simulation is complete.  For tick-by-tick control
/// (e.g., from a multi-machine World), use [`sim_scheduler_tick`] directly.
#[no_mangle]
pub unsafe extern "C" fn sim_start_scheduler() {
    let mut sim_time: Tick = 0;

    // FreeRTOS's vTaskStartScheduler calls portDISABLE_INTERRUPTS() before
    // xPortStartScheduler. Balance it here since our simulator doesn't use
    // real interrupt masking via the initial stack frame.
    unsafe {
        sim_exit_critical();
    }

    // Create Rust fibers for any TCBs registered via sim_port_task_created
    // (e.g., the timer daemon task and idle tasks created by FreeRTOS
    // inside vTaskStartScheduler).  These are deferred because creating
    // corosensei coroutines deep in FreeRTOS's call stack causes segfaults.
    unsafe {
        sim_bridge_create_pending_fibers();
    }

    while run_one_scheduler_cycle(&mut sim_time) {}
}

// ---------------------------------------------------------------------------
// C ABI: sim_scheduler_tick (single-cycle advancement)
// ---------------------------------------------------------------------------

/// Advance the FreeRTOS scheduler by one cycle and return.
///
/// On the first call from a given thread, performs the one-time scheduler
/// setup (critical section exit and deferred fiber creation).  Each call
/// executes exactly one scheduling decision: either resume a runnable task
/// (which runs until it yields, blocks, or exits) OR advance virtual time
/// to the next event boundary and wake any sleepers.
///
/// Returns 1 if the simulation has more work to do (runnable or sleeping
/// tasks remain), or 0 if the simulation is complete (no runnable tasks
/// and no sleeping tasks and no I/O progress).
///
/// # Safety
///
/// Must be called from the main scheduler context (not within a fiber).
/// The caller is responsible for calling this repeatedly until it returns 0.
/// Assumes the global trace sink has been initialized via `init_global`.
///
/// # Example (C)
///
/// ```c
/// // Tick-by-tick loop — equivalent to sim_start_scheduler().
/// while (sim_scheduler_tick()) {
///     // The caller can interleave its own work between ticks.
/// }
/// ```
#[no_mangle]
pub unsafe extern "C" fn sim_scheduler_tick() -> u32 {
    SCHEDULER_TICK_STATE.with(|state| {
        let mut s = state.borrow_mut();

        // One-time setup on first call from this thread.
        if !s.initialized {
            s.initialized = true;
            s.sim_time = 0;
            // FreeRTOS's vTaskStartScheduler calls portDISABLE_INTERRUPTS()
            // before xPortStartScheduler. Balance it here.
            unsafe {
                sim_exit_critical();
            }
            // Create Rust fibers for any TCBs deferred from C.
            unsafe {
                sim_bridge_create_pending_fibers();
            }
        }

        let mut sim_time = s.sim_time;
        let more = run_one_scheduler_cycle(&mut sim_time);
        s.sim_time = sim_time;

        // Flush thread-local trace (firmware sim_trace_u32 calls, can_send/recv, etc.)
        // into the active SimGlobal's trace sink so drain_trace_prefixed can see them.
        flush_trace();

        if more {
            1
        } else {
            0
        }
    })
}

/// Yield the currently executing task from C code.
///
/// Uses the TLS yielder directly — never touches the global RefCell.
/// This is safe to call from within a running fiber.
///
/// # Safety
///
/// Must be called from within a running fiber (i.e., while a coroutine
/// resume is in progress and the TLS yielder is set).  Calling from
/// outside a fiber records a fatal error but returns gracefully.
#[no_mangle]
pub unsafe extern "C" fn sim_port_yield() {
    let ok = suspend_active_fiber(YieldReason::RtosPortYield);
    if !ok {
        // Record fatal error via thread-local trace
        TL_TRACE.with(|tl| {
            tl.borrow_mut().push(sim_core::trace::TraceEvent::Fatal {
                at: SIM_NOW.load(Ordering::Relaxed),
                code: sim_core::error::SimErrorCode::YieldWithoutActiveFiber,
            });
        });
    }
}

/// Mark the current task as exited.
///
/// # Safety
///
/// Must be called from within a running fiber.  Suspends the fiber with
/// `TaskExit` reason; the scheduler will not resume it again.
#[no_mangle]
pub unsafe extern "C" fn sim_task_exit() {
    suspend_active_fiber(YieldReason::TaskExit);
}

/// Record that a task has been deleted by the RTOS kernel.
///
/// Called from within a fiber context (via `traceTASK_DELETE` hook during
/// `vTaskDelete`).  Unlike `sim_task_exit`, this does not suspend the
/// current fiber — it records the *target* task for deferred cleanup.
/// The target may be the current task (self-deletion) or another task.
///
/// The task ID is pushed to a thread-local list.  After the fiber yields,
/// `process_pending_deletions()` marks the task as `Exited` in the global
/// state, from a safe context where `SIM_GLOBAL` is not borrowed.
///
/// # Safety
///
/// Safe to call from any context (inside or outside a fiber).  Uses
/// thread-local storage exclusively.
#[no_mangle]
pub unsafe extern "C" fn sim_task_deleted(task_id: u64) {
    PENDING_DELETIONS.with(|pd| {
        pd.borrow_mut().push(task_id);
    });
}

/// Process pending task deletions recorded by `sim_task_deleted`.
///
/// Must be called from the scheduler main loop (or any context where
/// `SIM_GLOBAL` is safely accessible — i.e., NOT from within a fiber).
/// Drains the thread-local `PENDING_DELETIONS` list and marks each task
/// as `TaskState::Exited` in the global task registry.
///
/// The task's coroutine is leaked (via `ManuallyDrop`) to avoid
/// `force_unwind` panics: a deleted task's coroutine is suspended
/// inside an RTOS primitive (vTaskDelay, etc.) with no active yielder,
/// and `Coroutine::drop`'s force-unwind attempts to resume it.
/// Leaking is safe because this only happens at simulation end;
/// process exit reclaims all memory.
pub(crate) fn process_pending_deletions() {
    PENDING_DELETIONS.with(|pd| {
        let deleted_ids: Vec<u64> = pd.borrow_mut().drain(..).collect();
        if deleted_ids.is_empty() {
            return;
        }
        with_sim_global(|global| {
            let mut global = global.borrow_mut();
            // Use a set to avoid O(D × T) nested loop when many tasks are deleted.
            let deleted_set: std::collections::BTreeSet<_> = deleted_ids.iter().copied().collect();
            for task in global.tasks.iter_mut() {
                if deleted_set.contains(&task.id) {
                    task.mark_deleted();
                }
            }
        });
    });
}

/// Suspend the current task until an absolute virtual time.
///
/// # Safety
///
/// Must be called from within a running fiber.  The scheduler will
/// not resume this fiber before `until_ticks`.
#[no_mangle]
pub unsafe extern "C" fn sim_task_delay_until(until_ticks: u64) {
    suspend_active_fiber(YieldReason::SleepUntil(until_ticks));
}

/// Enter a virtual critical section.
///
/// Uses thread-local counter — safe to call from within a fiber.
///
/// # Safety
///
/// Always safe — only touches a thread-local counter.  Can be called
/// from any context.  Callers must pair with `sim_exit_critical`.
#[no_mangle]
pub unsafe extern "C" fn sim_enter_critical() {
    CRITICAL_NESTING.with(|c| {
        c.set(c.get().saturating_add(1));
    });
}

/// Exit a virtual critical section.
///
/// Uses thread-local counter — safe to call from within a fiber.
///
/// When the nesting count reaches zero, any deferred virtual interrupts
/// are delivered immediately.
///
/// # Safety
///
/// Always safe — only touches a thread-local counter.  Can be called
/// from any context.  Must be paired with a prior `sim_enter_critical`.
#[no_mangle]
pub unsafe extern "C" fn sim_exit_critical() {
    let was_locked = is_critical_locked();
    CRITICAL_NESTING.with(|c| {
        c.set(c.get().saturating_sub(1));
    });

    // If we just unlocked (was locked before decrement, now not locked),
    // deliver any pending IRQs that were deferred.
    if was_locked && !is_critical_locked() {
        let now = SIM_NOW.load(Ordering::Relaxed);
        deliver_pending_irqs(now);
    }
}

/// Whether virtual interrupts are currently locked.
pub fn is_critical_locked() -> bool {
    CRITICAL_NESTING.with(|c| c.get() > 0)
}

/// Record a u32 value in the trace.
///
/// Uses thread-local trace buffer — safe to call from within a fiber.
///
/// # Safety
///
/// `label_ptr` must be a valid null-terminated C string or null.
/// Safe to call from any context (uses thread-local buffer).
#[no_mangle]
pub unsafe extern "C" fn sim_trace_u32(label_ptr: *const std::ffi::c_char, value: u32) {
    let label = if label_ptr.is_null() {
        "?"
    } else {
        let c_str = std::ffi::CStr::from_ptr(label_ptr);
        c_str.to_str().unwrap_or("?")
    };
    let label_static: &'static str = Box::leak(label.to_string().into_boxed_str());

    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: SIM_NOW.load(Ordering::Relaxed),
            label: label_static,
            value,
        });
    });
}

// ---------------------------------------------------------------------------
// Public Rust API (for initialization)
// ---------------------------------------------------------------------------

/// Initialize the simulator global state with a trace sink.
pub fn init_global(trace: Box<TraceSink>) {
    with_sim_global(|global| {
        let mut global = global.borrow_mut();
        global.trace = Some(trace);
    });
    set_sim_now(0);
}

/// Access the global state for a read-only operation.
pub fn with_global<F, R>(f: F) -> R
where
    F: FnOnce(&SimGlobal) -> R,
{
    with_sim_global(|global| {
        let global = global.borrow();
        f(&global)
    })
}

/// Access the global state for a mutable operation.
pub fn with_global_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut SimGlobal) -> R,
{
    with_sim_global(|global| {
        let mut global = global.borrow_mut();
        f(&mut global)
    })
}

/// Flush the thread-local trace into the main trace sink.
pub fn flush_trace() {
    with_sim_global(|global| {
        let mut global = global.borrow_mut();
        TL_TRACE.with(|tl| {
            let mut tl = tl.borrow_mut();
            if !tl.is_empty() && global.trace.is_some() {
                global.trace.as_mut().unwrap().events.append(&mut tl);
            }
            tl.clear();
        });
    })
}

// ---------------------------------------------------------------------------
// Native Rust task API (§9)
// ---------------------------------------------------------------------------

/// Flush pending trace events to stdout before calling _exit().
///
/// This is called from nsi_shim.c's nsi_vprint_error_and_exit() before
/// _exit(0) to ensure trace events recorded inside fibers are printed.
/// nsi_vprint_error_and_exit is the normal termination path for Zephyr
/// apps whose main() returns (which triggers CODE_UNREACHABLE).
///
/// # Safety
///
/// Always safe — only accesses thread-local storage.  Can be called
/// from any context (including from within a C signal handler's exit path).
#[no_mangle]
pub unsafe extern "C" fn flush_trace_pending() {
    use std::io::Write;
    TL_TRACE.with(|tl| {
        let mut tl = tl.borrow_mut();
        for event in tl.drain(..) {
            // Write directly to stdout, bypassing the trace sink
            // which might also be unflushed.
            let _ = writeln!(std::io::stdout(), "{}", event);
        }
    });
    let _ = std::io::stdout().flush();
}

/// Context passed to the body of a Rust-native simulated task.
///
/// Provides the same primitives available to C tasks — yield, sleep,
/// and read virtual time — using the shared fiber runtime underneath.
///
/// This is the Rust equivalent of `sim_port_yield`, `sim_task_delay_until`,
/// and `sim_now_ticks` from the C ABI.
pub struct TaskContext {
    /// The task's unique identifier.
    pub task_id: TaskId,
}

impl TaskContext {
    /// Yield cooperatively, allowing other tasks to run.
    ///
    /// The scheduler may immediately resume this task if no higher-priority
    /// task is ready.
    pub fn yield_now(&self) {
        suspend_active_fiber(YieldReason::Cooperative);
    }

    /// Sleep until an absolute virtual time.
    ///
    /// The scheduler will not resume this task before `at` ticks.
    pub fn sleep_until(&self, at: Tick) {
        suspend_active_fiber(YieldReason::SleepUntil(at));
    }

    /// Sleep for a relative number of ticks from now.
    pub fn sleep_for(&self, delta: Tick) {
        let now = SIM_NOW.load(Ordering::Relaxed);
        self.sleep_until(now.saturating_add(delta));
    }

    /// Current virtual time in ticks.
    pub fn now(&self) -> Tick {
        SIM_NOW.load(Ordering::Relaxed)
    }
}

/// Spawn a Rust task that runs on the same fiber runtime as C tasks.
///
/// The closure `f` executes as the task body inside a stackful coroutine.
/// It receives a [`TaskContext`] for yield/sleep/time operations and can
/// call any re-entrant-safe C ABI function (trace, budget poll, etc.).
///
/// # Panics
///
/// Panics in the task body are caught by the scheduler's `catch_unwind`
/// boundary and mark the task as `Faulted` — they do not crash the
/// simulator process.
///
/// # Example
///
/// ```ignore
/// let id = spawn_rust_task("rust_blinker", 1, 4096, |ctx| {
///     for _ in 0..3 {
///         sim_trace_u32(b"rust_tick\0".as_ptr().cast(), 1);
///         ctx.sleep_for(2);
///     }
/// });
/// ```
pub fn spawn_rust_task<F>(name: &'static str, priority: u32, stack_size: usize, f: F) -> TaskId
where
    F: FnOnce(TaskContext) + Send + 'static,
{
    with_sim_global(|global| {
        let mut global = global.borrow_mut();

        let id = global.next_task_id;
        global.next_task_id += 1;

        // Convert requested stack bytes to words for Fiber::new.
        let requested_stack_words = (stack_size / std::mem::size_of::<usize>()) as u32;

        let fiber = Fiber::new(
            id,
            name,
            priority,
            requested_stack_words,
            sim_fiber::MIN_HOST_COROUTINE_STACK,
            id,
            move |_reason| {
                let ctx = TaskContext { task_id: id };
                f(ctx);
                suspend_active_fiber(YieldReason::TaskExit);
            },
        );
        global.tasks.push(fiber);
        id
    })
}

// ─────────────────────────────────────────────────────────────────────
// CPU-bound stall mitigation (function-entry budget)
// ─────────────────────────────────────────────────────────────────────

/// Poll the current task's function-entry budget.
///
/// Called from `__cyg_profile_func_enter` (emitted by -finstrument-functions)
/// and from the `SIM_LOOP_POLL()` manual loop hook.
///
/// `file` and `line` identify the call site (may be null/0 from the
/// automatic function-entry hook).  They are recorded in the trace when
/// the budget is exceeded.
///
/// # Safety
///
/// Must be called from within a running fiber.  Uses thread-local state
/// only (re-entrant safe).
#[no_mangle]
pub unsafe extern "C" fn sim_budget_poll(_file: *const std::ffi::c_char, line: u32) {
    let exceeded = BUDGET.with(|b| {
        let mut b = b.borrow_mut();
        b.entry_count += 1;
        if b.entry_count >= b.max_entries && !b.exceeded {
            b.exceeded = true;
            true
        } else {
            false
        }
    });

    if exceeded {
        // Reset the counter if we're inside a fiber (the yield will
        // succeed and the fiber resumes with a fresh budget).
        // Outside a fiber (e.g., unit test), leave the exceeded
        // state for inspection.
        if sim_fiber::has_active_fiber() {
            BUDGET.with(|b| {
                let mut b = b.borrow_mut();
                b.entry_count = 0;
                b.exceeded = false;
            });
        }

        let now = SIM_NOW.load(Ordering::Relaxed);
        TL_TRACE.with(|tl| {
            tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
                at: now,
                label: "budget_exceeded",
                value: line,
            });
        });

        suspend_active_fiber(YieldReason::BudgetExceeded);
    }
}

/// Reset the function-entry budget counter for the current task.
///
/// Called at task start to clear any residual budget state.
///
/// # Safety
///
/// Always safe — uses thread-local state only.
#[no_mangle]
pub unsafe extern "C" fn sim_budget_reset() {
    BUDGET.with(|b| {
        let mut b = b.borrow_mut();
        b.entry_count = 0;
        b.exceeded = false;
    });
}

/// Set the budget limit (max function/edge checks before forced yield).
///
/// For Tier 3 edge instrumentation, a much lower value (e.g., 10-100)
/// is recommended because sim_budget_poll is called every
/// EDGE_CHECK_INTERVAL edges.
///
/// # Safety
///
/// Always safe — uses thread-local state only.
#[no_mangle]
pub unsafe extern "C" fn sim_budget_set_limit(max_entries: u64) {
    BUDGET.with(|b| {
        b.borrow_mut().max_entries = max_entries;
    });
}

/// Poll host FDs and wake any blocked tasks whose FDs are ready.
///
/// Called by the scheduler when tasks are blocked on I/O and no
/// runnable tasks exist.
///
/// `next_virtual_event` is the absolute tick time of the next scheduled
/// virtual event, if any.  The poll timeout is clamped to the wall-clock
/// equivalent of the time until that event, so we don't oversleep past
/// a virtual deadline.  If there is no next event, a short default is used.
///
/// Returns the number of tasks woken.
///
/// On non-Unix platforms (Windows), host I/O is not supported so this
/// always returns 0.
#[cfg(unix)]
pub fn host_poll_and_wake(now: Tick, next_event: Option<Tick>) -> u32 {
    // Compute timeout: if there's a next virtual event, poll no longer
    // than the wall-clock equivalent of the time until that event.
    // Virtual ticks are nominally nanoseconds; the default rate is 1ms/tick.
    // For polling we use a lower bound of 0 and an upper bound of 100ms.
    let timeout = if let Some(deadline) = next_event {
        if deadline > now {
            let delta_ticks = deadline - now;
            // Virtual ticks are 1 ms each; convert to Duration.
            // Clamp to [0, 100] ms so we don't block the host too long.
            let ms = delta_ticks.clamp(0, 100);
            std::time::Duration::from_millis(ms)
        } else {
            std::time::Duration::ZERO
        }
    } else {
        // No virtual events pending — poll briefly.
        std::time::Duration::from_millis(100)
    };

    let ready =
        sim_net::host_poller::with_host_poller_mut(|hp| hp.poll(Some(timeout)).unwrap_or_default());

    let mut woken = 0u32;
    if let Some(ready_list) = ready {
        // Collect task IDs to wake (avoid double mutable borrow)
        let to_wake: Vec<u64> = ready_list.iter().map(|(_, tid)| *tid).collect();

        for task_id in &to_wake {
            // Wake the fiber associated with this task
            with_sim_global(|global| {
                let mut global = global.borrow_mut();
                for task in global.tasks.iter_mut() {
                    if task.id == *task_id && matches!(task.state, sim_fiber::TaskState::IoWaiting)
                    {
                        task.set_ready();
                        woken += 1;
                    }
                }
                // Record trace (separate from the iter_mut to avoid double borrow)
                if let Some(ref mut trace) = global.trace {
                    trace.record(sim_core::trace::TraceEvent::TaskResume {
                        at: now,
                        task: *task_id,
                        reason: "io_ready",
                    });
                }
            });

            // Note: fd cleanup is done in a separate pass below
        }

        // Clear ready flags and unblock task associations for all ready FDs
        for (fd, _task_id) in &ready_list {
            sim_net::host_poller::with_host_poller_mut(|hp| {
                hp.clear_ready(*fd);
                hp.unblock_task(*fd);
            });
        }
    }
    woken
}

/// Stub: host I/O not available on non-Unix platforms.
#[cfg(not(unix))]
pub fn host_poll_and_wake(_now: Tick, _next_event: Option<Tick>) -> u32 {
    0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_ffi::{
        sim_irq_deliver_pending, sim_irq_raise, sim_timer_arm, sim_uart_write,
    };
    use crate::net_ffi::{sim_net_drain_tx, sim_net_inject_rx, sim_net_poll};
    use sim_fiber::ResumeReason;
    // Network test needs smoltcp traits
    use sim_net::smoltcp::phy::{Device, RxToken, TxToken};
    use sim_net::smoltcp::time::Instant;

    #[test]
    fn test_create_task_returns_handle() {
        unsafe {
            let name = std::ffi::CString::new("test_task").unwrap();
            let handle = sim_create_task(
                name.as_ptr(),
                Some(test_entry),
                std::ptr::null_mut(),
                256,
                1,
            );
            assert!(handle > 0);
        }
    }

    unsafe extern "C" fn test_entry(_arg: *mut std::ffi::c_void) {
        // Minimal entry that just exits
    }

    #[test]
    fn test_critical_section_nesting() {
        unsafe {
            assert!(!is_critical_locked());
            sim_enter_critical();
            assert!(is_critical_locked());
            sim_enter_critical();
            sim_exit_critical();
            assert!(is_critical_locked());
            sim_exit_critical();
            assert!(!is_critical_locked());
        }
    }

    // ── Phase 10: Virtual device tests ────────────────────────────────

    /// Test that writing to a virtual UART records a trace event.
    #[test]
    fn test_uart_write_trace() {
        // Initialize global trace
        let trace = Box::new(sim_core::trace::TraceSink::new());
        init_global(trace);

        // Create a UART device
        let uart = sim_devices::VirtualUart::new(0, 115200);
        sim_devices::uart_insert(uart);

        // Write some bytes
        let data: [u8; 5] = [b'h', b'e', b'l', b'l', b'o'];
        unsafe {
            let written = sim_uart_write(0, data.as_ptr(), data.len() as u32);
            assert_eq!(written, 5);
        }

        // Flush TL trace
        flush_trace();

        // Check global trace for uart_tx event
        with_sim_global(|global| {
            let global = global.borrow();
            if let Some(ref trace) = global.trace {
                let uart_events: Vec<_> = trace
                    .events
                    .iter()
                    .filter(|e| {
                        matches!(
                            e,
                            sim_core::trace::TraceEvent::UserU32 {
                                label: "uart_tx",
                                ..
                            }
                        )
                    })
                    .collect();
                assert_eq!(uart_events.len(), 1);
            } else {
                panic!("trace not initialized");
            }
        });

        // Verify UART buffer contents
        let tx_data = sim_devices::with_uart_mut(0, |u| u.drain_tx()).unwrap();
        assert_eq!(tx_data, b"hello");

        // Verify last_tx
        let last = sim_devices::with_uart(0, |u| u.last_tx).unwrap();
        assert_eq!(last, Some(b'o'));
    }

    /// Test that a virtual timer fires and raises an IRQ at the right time.
    #[test]
    fn test_timer_interrupt_raised() {
        // Clear any pending IRQs from previous tests
        sim_devices::irq::with_irq_mut(|c| {
            c.take_pending();
        });

        // Create and arm a one-shot timer
        let timer = sim_devices::VirtualTimer::new_oneshot(0, 48);
        sim_devices::timer_insert(timer);

        set_sim_now(0);

        // Arm timer: fire after 10 ticks from now
        unsafe {
            sim_timer_arm(0, 10);
        }

        // Verify timer is armed
        let armed = sim_devices::with_timer_mut(0, |t| t.armed).unwrap();
        assert!(armed);

        // At time 5, timer should not be expired yet
        let expired = sim_devices::with_timer(0, |t| t.is_expired(5)).unwrap();
        assert!(!expired);

        // Drain expired timers at time 5 — should fire 0
        let fired = sim_devices::drain_expired_timers(5);
        assert_eq!(fired, 0);
        assert!(!sim_devices::irq::with_irq(|c| c.is_pending(48)));

        // At time 10, timer should be expired
        let expired = sim_devices::with_timer(0, |t| t.is_expired(10)).unwrap();
        assert!(expired);

        // Drain at time 10 — should fire 1 timer, raising IRQ 48
        let fired = sim_devices::drain_expired_timers(10);
        assert_eq!(fired, 1);
        assert!(sim_devices::irq::with_irq(|c| c.is_pending(48)));

        // One-shot timer should be disarmed after firing
        let armed = sim_devices::with_timer(0, |t| t.armed).unwrap();
        assert!(!armed);

        // Clean up
        sim_devices::irq::with_irq_mut(|c| {
            c.clear(48);
        });
    }

    /// Test that interrupts are deferred during critical sections.
    #[test]
    fn test_interrupt_deferred_during_critical_section() {
        // Clear pending IRQs
        sim_devices::irq::with_irq_mut(|c| {
            c.take_pending();
        });

        set_sim_now(100);

        // Enter critical section
        unsafe {
            sim_enter_critical();
        }
        assert!(is_critical_locked());

        // Raise an IRQ
        unsafe {
            sim_irq_raise(17);
        }

        // IRQ should be pending in the controller
        assert!(sim_devices::irq::with_irq(|c| c.is_pending(17)));

        // But delivery should be blocked by critical section
        let delivered = unsafe { sim_irq_deliver_pending(100) };
        assert_eq!(delivered, 0);
        // IRQ is still pending (not consumed)
        assert!(sim_devices::irq::with_irq(|c| c.is_pending(17)));

        // Exit critical section — this should trigger delivery
        unsafe {
            sim_exit_critical();
        }
        assert!(!is_critical_locked());

        // IRQ should now be delivered (consumed by sim_exit_critical)
        assert!(!sim_devices::irq::with_irq(|c| c.is_pending(17)));
    }

    /// Test that IRQ delivery works when not in critical section.
    #[test]
    fn test_irq_delivered_when_not_locked() {
        // Clear pending IRQs
        sim_devices::irq::with_irq_mut(|c| {
            c.take_pending();
        });

        set_sim_now(200);

        assert!(!is_critical_locked());

        // Raise an IRQ
        unsafe {
            sim_irq_raise(33);
        }
        assert!(sim_devices::irq::with_irq(|c| c.is_pending(33)));

        // Delivery should succeed immediately
        let delivered = unsafe { sim_irq_deliver_pending(200) };
        assert_eq!(delivered, 1);

        // IRQ should be consumed
        assert!(!sim_devices::irq::with_irq(|c| c.is_pending(33)));
    }

    // ── Phase 11: Networking tests ─────────────────────────────────────

    /// Test that packet injection and drain produce trace events.
    #[test]
    fn test_net_inject_and_drain_traces() {
        let trace = Box::new(sim_core::trace::TraceSink::new());
        init_global(trace);

        // Register a network device
        sim_net::net_device_insert(sim_net::SimNetDevice::new(1500));

        // Inject a packet
        let pkt: [u8; 10] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a];
        unsafe {
            let injected = sim_net_inject_rx(pkt.as_ptr(), pkt.len() as u32);
            assert_eq!(injected, 10);
        }

        // Poll should see data
        unsafe {
            assert_eq!(sim_net_poll(), 1);
        }

        // Receive and transmit via smoltcp Device trait
        sim_net::with_net_device_mut(|dev| {
            let ts = Instant::from_micros_const(0);
            let result = dev.receive(ts);
            assert!(result.is_some());

            let (rx_token, tx_token) = result.unwrap();
            rx_token.consume(|data| {
                assert_eq!(data.len(), 10);
            });

            // Transmit back
            tx_token.consume(5, |buf| {
                buf.copy_from_slice(&pkt[..5]);
            });
        })
        .unwrap();

        // Drain tx
        let mut tx_buf = [0u8; 100];
        unsafe {
            let drained = sim_net_drain_tx(tx_buf.as_mut_ptr(), tx_buf.len() as u32);
            assert_eq!(drained, 5);
            assert_eq!(&tx_buf[..5], &pkt[..5]);
        }

        // Flush trace
        flush_trace();

        // Verify trace has PacketRx and PacketTx events
        with_sim_global(|global| {
            let global = global.borrow();
            if let Some(ref trace) = global.trace {
                let rx_count = trace
                    .events
                    .iter()
                    .filter(|e| matches!(e, sim_core::trace::TraceEvent::PacketRx { .. }))
                    .count();
                let tx_count = trace
                    .events
                    .iter()
                    .filter(|e| matches!(e, sim_core::trace::TraceEvent::PacketTx { .. }))
                    .count();
                assert_eq!(rx_count, 1, "expected 1 PacketRx event");
                assert_eq!(tx_count, 1, "expected 1 PacketTx event");
            } else {
                panic!("trace not initialized");
            }
        });
    }

    // ── Budget / instrumentation tests ────────────────────────────────

    /// Test that sim_budget_poll increments the counter and that
    /// sim_budget_reset clears it.  Does not run inside a fiber,
    /// so the yield path is only tested indirectly (counter logic).
    #[test]
    fn test_budget_counter() {
        unsafe {
            sim_budget_reset();
        }

        // Verify initial counter is 0
        BUDGET.with(|b| {
            assert_eq!(b.borrow().entry_count, 0);
            assert!(!b.borrow().exceeded);
        });

        // Call sim_budget_poll up to the limit (1M by default).
        // We set a small limit for testing.
        BUDGET.with(|b| {
            b.borrow_mut().max_entries = 5;
        });

        // First 4 calls should not exceed
        for _ in 0..4 {
            unsafe {
                sim_budget_poll(std::ptr::null(), 0);
            }
        }
        BUDGET.with(|b| {
            assert_eq!(b.borrow().entry_count, 4);
            assert!(!b.borrow().exceeded);
        });

        // 5th call should set exceeded flag
        unsafe {
            sim_budget_poll(std::ptr::null(), 0);
        }
        BUDGET.with(|b| {
            assert_eq!(b.borrow().entry_count, 5);
            assert!(b.borrow().exceeded);
        });

        // sim_budget_reset clears everything
        unsafe {
            sim_budget_reset();
        }
        BUDGET.with(|b| {
            assert_eq!(b.borrow().entry_count, 0);
            assert!(!b.borrow().exceeded);
        });
    }

    // ── Rust task API tests ─────────────────────────────────────────

    /// Test that spawn_rust_task registers a fiber and the task body
    /// can yield, sleep, and read virtual time via TaskContext.
    #[test]
    fn test_rust_task_yield_and_sleep() {
        // Initialize a trace sink so suspend_active_fiber works.
        let trace = Box::new(sim_core::trace::TraceSink::new());
        init_global(trace);

        // Spawn a Rust task.
        let task_id = spawn_rust_task("rust_tester", 1, 4096, |ctx| {
            // Verify we can read virtual time.
            let t0 = ctx.now();
            assert_eq!(t0, 0);

            // Yield cooperatively (once).
            ctx.yield_now();

            // Sleep for 3 ticks.
            ctx.sleep_for(3);

            // After sleep, we should resume here.
            // (In a real scheduler, the scheduler would set SIM_NOW.)
        });

        assert!(task_id > 0);

        // The task should be registered.
        let task_count = with_global(|g| g.tasks.len());
        assert_eq!(task_count, 1);

        // Reset virtual time right before resume to avoid race with
        // other test threads that modify the global SIM_NOW atomic.
        set_sim_now(0);

        // Manually resume the fiber steps.
        // Step 1: Start → should yield cooperatively.
        let (reason, _) = SIM_GLOBAL.with(|global| {
            let mut global = global.borrow_mut();
            let task = &mut global.tasks[0];
            let result = task.resume(ResumeReason::Start);
            (result, task.state)
        });
        assert_eq!(reason, Some(YieldReason::Cooperative));

        // Step 2: Resume → should sleep for 3.
        let reason = SIM_GLOBAL.with(|global| {
            let mut global = global.borrow_mut();
            let task = &mut global.tasks[0];
            task.resume(ResumeReason::SchedulerSelected)
        });
        assert_eq!(reason, Some(YieldReason::SleepUntil(3)));

        // Step 3: After sleep, wake and resume → task exits.
        with_sim_global(|global| {
            let mut global = global.borrow_mut();
            let task = &mut global.tasks[0];
            task.try_wake(3);
        });
        let reason = SIM_GLOBAL.with(|global| {
            let mut global = global.borrow_mut();
            let task = &mut global.tasks[0];
            task.resume(ResumeReason::TimeoutExpired)
        });
        assert_eq!(reason, Some(YieldReason::TaskExit));
        assert!(with_global(|g| g.tasks[0].is_terminated()));
    }

    /// Test that a Rust task that panics is caught and marked Faulted.
    #[test]
    fn test_rust_task_panic_is_faulted() {
        let _trace = Box::new(sim_core::trace::TraceSink::new());
        init_global(Box::new(sim_core::trace::TraceSink::new()));

        spawn_rust_task("panicker", 1, 4096, |_ctx| {
            panic!("deliberate panic in rust task");
        });

        // Resume the fiber inside catch_unwind (as the scheduler does).
        let panicked = SIM_GLOBAL.with(|global| {
            let mut global = global.borrow_mut();
            let task = &mut global.tasks[0];
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                task.resume(ResumeReason::Start)
            }));
            match result {
                Ok(_) => false,
                Err(_) => {
                    task.state = sim_fiber::TaskState::Faulted;
                    true
                }
            }
        });

        assert!(panicked);
        assert!(with_global(|g| g.tasks[0].is_terminated()));
    }
}

// ---------------------------------------------------------------------------
// Event queue for virtual peripherals (RTOS-agnostic)
// ---------------------------------------------------------------------------

use std::collections::BTreeMap;

thread_local! {
    /// Peripheral event queue: maps absolute cycle time → list of C callbacks.
    /// Owned by the costar engine, not by any RTOS.  Virtual devices
    /// (UART, timer, GPIO) schedule events here via `sim_schedule_event`.
    /// The drain loop dispatches them when virtual time reaches the deadline.
    pub(crate) static EVENT_QUEUE: RefCell<BTreeMap<u64, Vec<unsafe extern "C" fn()>>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// Schedule a peripheral event callback at the given absolute cycle time.
///
/// This is the C ABI entry point for virtual devices.  The callback is a
/// C function pointer (typically a thin wrapper that calls `sim_irq_raise`
/// or similar).  The drain loop will invoke all callbacks at `at_cycles`
/// when virtual time reaches that point.
///
/// # Safety
///
/// `callback` must point to a valid function with C ABI.  Can be called
/// from any context (inside or outside a fiber).
#[no_mangle]
pub unsafe extern "C" fn sim_schedule_event(
    at_cycles: u64,
    callback: Option<unsafe extern "C" fn()>,
) {
    let cb = callback.expect("sim_schedule_event: NULL callback");
    EVENT_QUEUE.with(|q| {
        q.borrow_mut().entry(at_cycles).or_default().push(cb);
    });
}

/// Peek the next event deadline from the peripheral event queue.
///
/// Returns `None` if the queue is empty, or `Some(cycle_time)` of the
/// earliest pending event.  The drain loop uses this alongside the RTOS
/// timeout to decide how far to advance virtual time.
pub fn next_event_deadline() -> Option<u64> {
    EVENT_QUEUE.with(|q| {
        let q = q.borrow();
        q.keys().next().copied()
    })
}

/// Dispatch all peripheral callbacks at or before `now_cycles`.
///
/// Removes and invokes all callbacks with deadlines ≤ `now_cycles`.
/// Callbacks run with `catch_unwind` so a panicking peripheral doesn't
/// take down the whole simulation.
pub fn dispatch_events(now_cycles: u64) {
    // Update SIM_NOW so trace timestamps from within callbacks are correct.
    set_sim_now(now_cycles);
    loop {
        let batch: Option<Vec<unsafe extern "C" fn()>> = EVENT_QUEUE.with(|q| {
            let mut q = q.borrow_mut();
            // Pop the earliest key if it's ≤ now.
            let first_key = q.keys().next().copied();
            match first_key {
                Some(k) if k <= now_cycles => q.remove(&k),
                _ => None,
            }
        });
        match batch {
            Some(callbacks) => {
                for cb in callbacks {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                        cb();
                    }));
                }
            }
            None => break,
        }
    }
}
