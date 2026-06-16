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

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

use sim_core::time::Tick;
use sim_core::trace::TraceSink;
use sim_fiber::yield_reason::YieldReason;
use sim_fiber::{suspend_active_fiber, Fiber, TaskId};

pub mod simulator;

// ── C functions called FROM Rust (implemented in task.c) ──────────

#[link(name = "embedded_c_payload", kind = "static")]
extern "C" {
    fn sim_set_current_task_by_id(task_id: u64);
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
static SIM_NOW: AtomicU64 = AtomicU64::new(0);

/// Current task ID of the executing fiber, if any.
/// Atomic so it can be read from within a fiber (e.g., by
/// `sim_host_block_on_fd`) without touching the global RefCell.
/// Set by the scheduler before resuming a fiber, cleared after.
static CURRENT_TASK_ID: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static CRITICAL_NESTING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

thread_local! {
    static TL_TRACE: RefCell<Vec<sim_core::trace::TraceEvent>> =
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
    static BUDGET: std::cell::RefCell<BudgetState> =
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
    SIM_GLOBAL.with(|global| {
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
        id as usize
    })
}

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

    loop {
        // ── Compute earliest sleeping task wake time ──────────────
        let next_wake: Option<Tick> = SIM_GLOBAL.with(|global| {
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
        let task_idx: Option<usize> = SIM_GLOBAL.with(|global| {
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
                let task_id = SIM_GLOBAL.with(|global| {
                    let mut global = global.borrow_mut();
                    global.current_task = Some(idx);
                    let tid = global.tasks[idx].id;
                    if let Some(ref mut trace) = global.trace {
                        trace.record(sim_core::trace::TraceEvent::TaskResume {
                            at: sim_time,
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
                let (yield_reason, panicked) = SIM_GLOBAL.with(|global| {
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
                SIM_GLOBAL.with(|global| {
                    let mut global = global.borrow_mut();
                    if let Some(reason) = yield_reason {
                        if let Some(ref mut trace) = global.trace {
                            if panicked {
                                // Record the fatal panic event.
                                trace.record(sim_core::trace::TraceEvent::Fatal {
                                    at: sim_time,
                                    code: sim_core::error::SimErrorCode::PanicCrossedCAbi,
                                });
                            }
                            let reason_str: &'static str =
                                Box::leak(format!("{:?}", reason).into_boxed_str());
                            trace.record(sim_core::trace::TraceEvent::TaskYield {
                                at: sim_time,
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
                deliver_pending_irqs(sim_time);

                set_sim_now(sim_time);
            }
            None => {
                // ── No runnable task ──────────────────────────
                //
                // Check the peripheral event queue alongside the
                // next RTOS wake time.  If a peripheral event is
                // sooner, advance to it and dispatch the callback
                // before processing RTOS timeouts.
                let event_deadline = next_event_deadline();

                match next_wake {
                    Some(wake_time) if wake_time > sim_time => {
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
                                sim_time = ev;
                                set_sim_now(sim_time);
                                dispatch_events(sim_time);
                                deliver_pending_irqs(sim_time);
                            }
                        }

                        // Advance ticks to the RTOS wake time.
                        let ticks_to_advance = (wake_time - sim_time) as u32;
                        if ticks_to_advance > 0 {
                            sim_time = wake_time;
                            unsafe {
                                sim_advance_ticks(ticks_to_advance);
                            }
                        }
                        // Deliver timer IRQs that may have fired during
                        // the advance.
                        deliver_pending_irqs(sim_time);

                        // Wake fibers whose sleep time has passed.
                        SIM_GLOBAL.with(|global| {
                            let mut global = global.borrow_mut();
                            for task in global.tasks.iter_mut() {
                                task.try_wake(sim_time);
                            }
                        });

                        // Deliver IRQs that may have been deferred.
                        deliver_pending_irqs(sim_time);

                        // Also dispach any events at the new time.
                        dispatch_events(sim_time);
                        deliver_pending_irqs(sim_time);

                        // Also poll host FDs ...
                        let next_wake_after = SIM_GLOBAL.with(|global| {
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
                        let io_waiting_after = SIM_GLOBAL.with(|global| {
                            let global = global.borrow();
                            global
                                .tasks
                                .iter()
                                .any(|t| matches!(t.state, sim_fiber::TaskState::IoWaiting))
                        });
                        if io_waiting_after {
                            host_poll_and_wake(sim_time, next_wake_after);
                            deliver_pending_irqs(sim_time);
                        }

                        set_sim_now(sim_time);
                    }
                    _ => {
                        // ── No sleeping tasks — check for I/O-blocked tasks ─
                        let io_waiting = SIM_GLOBAL.with(|global| {
                            let global = global.borrow();
                            global
                                .tasks
                                .iter()
                                .any(|t| matches!(t.state, sim_fiber::TaskState::IoWaiting))
                        });

                        if io_waiting {
                            // Some tasks are blocked on host I/O.  Poll
                            // host sockets and wake any whose FDs are ready.
                            let woken = host_poll_and_wake(sim_time, next_wake);
                            if woken > 0 {
                                // Tasks were woken — loop back to run them.
                                set_sim_now(sim_time);
                                continue;
                            }
                        }

                        // No sleeping tasks and no I/O progress — simulation complete.
                        break;
                    }
                }
            }
        }
    }
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
    SIM_GLOBAL.with(|global| {
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
    SIM_GLOBAL.with(|global| {
        let global = global.borrow();
        f(&global)
    })
}

/// Access the global state for a mutable operation.
pub fn with_global_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut SimGlobal) -> R,
{
    SIM_GLOBAL.with(|global| {
        let mut global = global.borrow_mut();
        f(&mut global)
    })
}

/// Flush the thread-local trace into the main trace sink.
pub fn flush_trace() {
    SIM_GLOBAL.with(|global| {
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

/// Diagnostic-only: trace a u32 label+value directly to stderr for debugging.
/// NOT part of the normal trace infrastructure — bypasses TL_TRACE and the trace sink.
#[allow(dead_code)]
pub fn trace_u32_raw(label: &str, value: u32) {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr(),
        "DIAG: {} ticks={} sim_time_now={}",
        label,
        value,
        SIM_NOW.load(Ordering::Relaxed)
    );
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
    SIM_GLOBAL.with(|global| {
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

// ---------------------------------------------------------------------------
// Virtual device C ABI exports
// ---------------------------------------------------------------------------

/// Raise a virtual interrupt.
///
/// Records the event in the trace and adds the IRQ to the pending set.
/// Actual delivery happens when `sim_irq_deliver_pending()` is called
/// from a non-critical context.
///
/// # Safety
///
/// Always safe — only touches the thread-local IRQ controller and trace.
/// Can be called from any context (within a fiber, from C, etc.).
#[no_mangle]
pub unsafe extern "C" fn sim_irq_raise(irq: u32) {
    let now = SIM_NOW.load(Ordering::Relaxed);

    // Record in trace
    TL_TRACE.with(|tl| {
        tl.borrow_mut()
            .push(sim_core::trace::TraceEvent::InterruptRaised { at: now, irq });
    });

    // Add to IRQ controller
    sim_devices::irq::with_irq_mut(|ctrl| {
        ctrl.raise(irq);
    });
}

/// Clear a pending virtual interrupt (e.g., acknowledged by handler).
///
/// # Safety
///
/// Always safe — only touches the thread-local IRQ controller.
#[no_mangle]
pub unsafe extern "C" fn sim_irq_clear(irq: u32) {
    sim_devices::irq::with_irq_mut(|ctrl| {
        ctrl.clear(irq);
    });
}

/// Check whether any virtual interrupt is pending.
///
/// Returns the lowest pending IRQ number, or `u32::MAX` if none are pending.
///
/// # Safety
///
/// Always safe — only reads the thread-local IRQ controller.
#[no_mangle]
pub unsafe extern "C" fn sim_irq_pending() -> u32 {
    sim_devices::irq::with_irq(|ctrl| ctrl.peek_pending().first().copied().unwrap_or(u32::MAX))
}

/// Deliver all pending virtual interrupts, if not in a critical section.
///
/// Returns the number of interrupts delivered.  Each delivered interrupt
/// records an `InterruptDelivered` trace event.
///
/// Called by the scheduler loop between task resumptions.
///
/// # Safety
///
/// Safe — only touches thread-local state.
#[no_mangle]
pub unsafe extern "C" fn sim_irq_deliver_pending(now: u64) -> u32 {
    if is_critical_locked() {
        return 0;
    }

    let irqs = sim_devices::irq::with_irq_mut(|ctrl| ctrl.take_pending());

    let count = irqs.len() as u32;
    for irq in irqs {
        // Record delivery in trace
        TL_TRACE.with(|tl| {
            tl.borrow_mut()
                .push(sim_core::trace::TraceEvent::InterruptDelivered { at: now, irq });
        });
    }
    count
}

/// Deliver pending IRQs and drain expired timers.
///
/// Called by the scheduler after each task yield.
pub fn deliver_pending_irqs(now: u64) -> u32 {
    // Drain expired timers first (which may raise IRQs)
    sim_devices::drain_expired_timers(now);

    // Then deliver pending IRQs
    unsafe { sim_irq_deliver_pending(now) }
}

/// Write bytes to a virtual UART.
///
/// Returns the number of bytes actually written.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local UART map).
#[no_mangle]
pub unsafe extern "C" fn sim_uart_write(id: u32, data_ptr: *const u8, len: u32) -> u32 {
    if data_ptr.is_null() || len == 0 {
        return 0;
    }

    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };

    let now = SIM_NOW.load(Ordering::Relaxed);

    // Record in trace
    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "uart_tx",
            value: id,
        });
    });

    sim_devices::with_uart_mut(id, |uart| uart.write(data)).unwrap_or(0) as u32
}

/// Arm a virtual timer to fire after `delay_ticks` from the current time.
///
/// If the timer was already armed, the previous schedule is overwritten.
///
/// # Safety
///
/// Always safe — uses atomic time read and thread-local timer storage.
#[no_mangle]
pub unsafe extern "C" fn sim_timer_arm(id: u32, delay_ticks: u64) {
    let now = SIM_NOW.load(Ordering::Relaxed);
    sim_devices::with_timer_mut(id, |timer| {
        timer.arm(now, delay_ticks);
    });
}

/// Disarm a virtual timer.  No interrupt will fire.
///
/// # Safety
///
/// Always safe — uses thread-local timer storage.
#[no_mangle]
pub unsafe extern "C" fn sim_timer_disarm(id: u32) {
    sim_devices::with_timer_mut(id, |timer| {
        timer.disarm();
    });
}

/// Set a GPIO pin state.
///
/// Returns the IRQ number if the change triggered an interrupt, or
/// `u32::MAX` if no interrupt was triggered.
///
/// # Safety
///
/// Always safe — uses thread-local GPIO storage.
#[no_mangle]
pub unsafe extern "C" fn sim_gpio_set(id: u32, pin: u32, state: u32) -> u32 {
    let result = sim_devices::with_gpio_mut(id, |gpio| gpio.set(pin as usize, state != 0));
    match result {
        Some(Some(irq)) => {
            // GPIO change triggered an IRQ — raise it
            sim_irq_raise(irq);
            irq
        }
        _ => u32::MAX,
    }
}

// ---------------------------------------------------------------------------
// Virtual I2C C ABI exports (Phase 22)
// ---------------------------------------------------------------------------

/// Write data to an I2C target from the master.
///
/// The target address must have been set with `sim_i2c_set_address` first.
/// Returns the number of bytes written, or 0 if the controller is disabled
/// or not registered.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local I2C storage).
#[no_mangle]
pub unsafe extern "C" fn sim_i2c_write(id: u32, data_ptr: *const u8, len: u32) -> u32 {
    if data_ptr.is_null() || len == 0 {
        return 0;
    }
    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };
    let now = SIM_NOW.load(Ordering::Relaxed);

    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "i2c_write",
            value: id,
        });
    });

    sim_devices::with_i2c_mut(id, |i2c| i2c.write(data)).unwrap_or(0) as u32
}

/// Read data from an I2C target into a caller-provided buffer.
///
/// The target address must have been set with `sim_i2c_set_address` first.
/// Returns the number of bytes read.  The RX buffer must be pre-populated
/// via test-script injection.
///
/// # Safety
///
/// `buf_ptr` must be a valid pointer to at least `len` bytes of writable memory.
/// Safe to call from any context (uses thread-local I2C storage).
#[no_mangle]
pub unsafe extern "C" fn sim_i2c_read(id: u32, buf_ptr: *mut u8, len: u32) -> u32 {
    if buf_ptr.is_null() || len == 0 {
        return 0;
    }

    // Fault injection: check if a NACK was injected
    if sim_devices::with_fault_injector_mut(|f| f.consume_i2c_nack()) {
        return 0;
    }

    let now = SIM_NOW.load(Ordering::Relaxed);

    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "i2c_read",
            value: id,
        });
    });

    let result = sim_devices::with_i2c_mut(id, |i2c| i2c.read(len as usize));
    match result {
        Some(data) => {
            let actual = data.len().min(len as usize);
            let buf = unsafe { std::slice::from_raw_parts_mut(buf_ptr, actual) };
            buf.copy_from_slice(&data[..actual]);
            actual as u32
        }
        None => 0,
    }
}

/// Perform a combined I2C write-then-read (repeated start).
///
/// Writes `tx_len` bytes from `tx_ptr`, then reads `rx_len` bytes into
/// `rx_buf`.  Returns the number of bytes read, or 0 if the controller
/// is not found.
///
/// # Safety
///
/// `tx_ptr` must be valid for `tx_len` bytes.  `rx_buf` must be writable
/// for at least `rx_len` bytes.
/// Safe to call from any context (uses thread-local I2C storage).
#[no_mangle]
pub unsafe extern "C" fn sim_i2c_write_read(
    id: u32,
    tx_ptr: *const u8,
    tx_len: u32,
    rx_buf: *mut u8,
    rx_len: u32,
) -> u32 {
    if tx_ptr.is_null() || rx_buf.is_null() || tx_len == 0 || rx_len == 0 {
        return 0;
    }
    let tx_data = unsafe { std::slice::from_raw_parts(tx_ptr, tx_len as usize) };
    let now = SIM_NOW.load(Ordering::Relaxed);

    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "i2c_wr",
            value: id,
        });
    });

    let result = sim_devices::with_i2c_mut(id, |i2c| i2c.write_read(tx_data, rx_len as usize));
    match result {
        Some((_written, rx_data)) => {
            let actual = rx_data.len().min(rx_len as usize);
            let buf = unsafe { std::slice::from_raw_parts_mut(rx_buf, actual) };
            buf.copy_from_slice(&rx_data[..actual]);
            actual as u32
        }
        None => 0,
    }
}

/// Set the I2C target address.
///
/// # Safety
///
/// Always safe — uses thread-local I2C storage.
#[no_mangle]
pub unsafe extern "C" fn sim_i2c_set_address(id: u32, address: u16, ten_bit: u32) {
    sim_devices::with_i2c_mut(id, |i2c| {
        i2c.set_address(address, ten_bit != 0);
    });
}

/// Check whether the last I2C operation received a NACK.
///
/// Returns 1 if NACK was received, 0 otherwise (or if controller not found).
///
/// # Safety
///
/// Always safe — uses thread-local I2C storage.
#[no_mangle]
pub unsafe extern "C" fn sim_i2c_get_nack(id: u32) -> u32 {
    sim_devices::with_i2c(id, |i2c| i2c.nack as u32).unwrap_or(0)
}

/// Inject bytes into the I2C RX buffer (for test scripts).
///
/// This simulates an I2C target device sending data to the master.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local I2C storage).
#[no_mangle]
pub unsafe extern "C" fn sim_i2c_inject_rx(id: u32, data_ptr: *const u8, len: u32) {
    if data_ptr.is_null() || len == 0 {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };
    sim_devices::with_i2c_mut(id, |i2c| {
        i2c.inject_rx(data);
    });
}

// ---------------------------------------------------------------------------
// Virtual SPI C ABI exports (Phase 22)
// ---------------------------------------------------------------------------

/// Perform a full-duplex SPI transfer.
///
/// Writes `tx_len` bytes from `tx_ptr`, reads into `rx_buf` (up to `rx_len`
/// bytes).  Returns the number of bytes received.  The RX buffer should
/// be pre-populated via `sim_spi_inject_rx` for deterministic tests.
///
/// # Safety
///
/// `tx_ptr` must be valid for `tx_len` bytes.  `rx_buf` must be writable
/// for at least `rx_len` bytes.
/// Safe to call from any context (uses thread-local SPI storage).
#[no_mangle]
pub unsafe extern "C" fn sim_spi_transfer(
    id: u32,
    tx_ptr: *const u8,
    tx_len: u32,
    rx_buf: *mut u8,
    rx_len: u32,
) -> u32 {
    if tx_ptr.is_null() || rx_buf.is_null() || tx_len == 0 || rx_len == 0 {
        return 0;
    }
    let tx_data = unsafe { std::slice::from_raw_parts(tx_ptr, tx_len as usize) };
    let now = SIM_NOW.load(Ordering::Relaxed);

    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "spi_xfer",
            value: id,
        });
    });

    let result = sim_devices::with_spi_mut(id, |spi| spi.transfer(tx_data));
    match result {
        Some(rx_data) => {
            let actual = rx_data.len().min(rx_len as usize);
            let buf = unsafe { std::slice::from_raw_parts_mut(rx_buf, actual) };
            buf.copy_from_slice(&rx_data[..actual]);

            // Fault injection: corrupt first byte if SPI error was injected
            if sim_devices::with_fault_injector_mut(|f| f.consume_spi_error()) {
                buf[0] ^= 0xFF;
            }

            actual as u32
        }
        None => 0,
    }
}

/// Set SPI configuration: mode (0-3), clock speed (Hz), and word size (8 or 16).
///
/// # Safety
///
/// Always safe — uses thread-local SPI storage.
#[no_mangle]
pub unsafe extern "C" fn sim_spi_set_config(
    id: u32,
    mode: u32,
    speed_hz: u32,
    word_size: u32,
) -> u32 {
    let spi_mode = match mode {
        0 => sim_devices::SpiMode::Mode0,
        1 => sim_devices::SpiMode::Mode1,
        2 => sim_devices::SpiMode::Mode2,
        3 => sim_devices::SpiMode::Mode3,
        _ => return 1, // invalid mode
    };
    if word_size != 8 && word_size != 16 {
        return 2; // invalid word size
    }
    sim_devices::with_spi_mut(id, |spi| {
        spi.set_mode(spi_mode);
        spi.speed_hz = speed_hz;
        spi.set_word_size(word_size as u8);
    });
    0
}

/// Set SPI chip select state.
///
/// Returns 0 on success, 1 if controller not found.
///
/// # Safety
///
/// Always safe — uses thread-local SPI storage.
#[no_mangle]
pub unsafe extern "C" fn sim_spi_set_cs(id: u32, active: u32) -> u32 {
    let found = sim_devices::with_spi_mut(id, |spi| {
        spi.set_cs(active != 0);
    });
    if found.is_some() {
        0
    } else {
        1
    }
}

/// Inject bytes into the SPI RX buffer (for test scripts).
///
/// This simulates an SPI peripheral device sending data to the master.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local SPI storage).
#[no_mangle]
pub unsafe extern "C" fn sim_spi_inject_rx(id: u32, data_ptr: *const u8, len: u32) {
    if data_ptr.is_null() || len == 0 {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };
    sim_devices::with_spi_mut(id, |spi| {
        spi.inject_rx(data);
    });
}

// ---------------------------------------------------------------------------
// Virtual CAN C ABI exports (Phase 23)
// ---------------------------------------------------------------------------

/// Send a CAN frame from the specified controller.
///
/// If loopback mode is enabled on the controller, the frame is also
/// placed in the RX queue.  A `can_send` trace event is recorded.
///
/// Returns 0 on success, 1 if controller not found or send failed.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local CAN storage).
#[no_mangle]
pub unsafe extern "C" fn sim_can_send(
    ctrl_id: u32,
    can_id: u32,
    data_ptr: *const u8,
    len: u32,
    is_ext: u32,
    is_remote: u32,
) -> u32 {
    let dlc = len.min(8) as u8;
    let mut frame = if is_remote != 0 {
        sim_devices::CanFrame::new_remote(can_id, is_ext != 0)
    } else if is_ext != 0 {
        sim_devices::CanFrame::new_data_ext(can_id, &[])
    } else {
        sim_devices::CanFrame::new_data(can_id, &[])
    };
    frame.is_remote = is_remote != 0;
    frame.dlc = dlc;

    if !data_ptr.is_null() && len > 0 && is_remote == 0 {
        let data = unsafe { std::slice::from_raw_parts(data_ptr, dlc as usize) };
        frame.data[..dlc as usize].copy_from_slice(data);
    }

    let now = SIM_NOW.load(Ordering::Relaxed);
    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "can_send",
            value: ctrl_id,
        });
    });

    let ok = sim_devices::with_can_mut(ctrl_id, |can| can.send(frame)).unwrap_or(false);
    if ok {
        0
    } else {
        1
    }
}

/// Receive the oldest CAN frame from the RX queue.
///
/// Writes the frame payload into `buf` (up to `buf_len` bytes).  Writes the
/// CAN ID into `can_id_out`, the extended flag into `is_ext_out` (1 = extended),
/// and the remote flag into `is_remote_out` (1 = RTR).  A `can_recv` trace
/// event is recorded.
///
/// Returns the data length (DLC) of the received frame, or 0 if no frame
/// is available or the controller is not found.
///
/// # Safety
///
/// `buf` must be writable for at least `buf_len` bytes.  `can_id_out`,
/// `is_ext_out`, and `is_remote_out` must be valid pointers to u32.
/// Safe to call from any context (uses thread-local CAN storage).
#[no_mangle]
pub unsafe extern "C" fn sim_can_recv(
    ctrl_id: u32,
    buf: *mut u8,
    buf_len: u32,
    can_id_out: *mut u32,
    is_ext_out: *mut u32,
    is_remote_out: *mut u32,
) -> u32 {
    let now = SIM_NOW.load(Ordering::Relaxed);
    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "can_recv",
            value: ctrl_id,
        });
    });

    let result = sim_devices::with_can_mut(ctrl_id, |can| can.recv());
    match result {
        Some(Some(frame)) => {
            if !can_id_out.is_null() {
                unsafe { *can_id_out = frame.id };
            }
            if !is_ext_out.is_null() {
                unsafe { *is_ext_out = frame.is_extended as u32 };
            }
            if !is_remote_out.is_null() {
                unsafe { *is_remote_out = frame.is_remote as u32 };
            }
            let actual = (frame.dlc as usize).min(buf_len as usize);
            if !buf.is_null() && actual > 0 {
                let out = unsafe { std::slice::from_raw_parts_mut(buf, actual) };
                out.copy_from_slice(&frame.data[..actual]);
            }
            frame.dlc as u32
        }
        _ => 0,
    }
}

/// Inject a CAN frame into the RX queue (simulates an external node).
///
/// Places a frame with the given ID, data, and flags into the controller's
/// RX queue.  A `can_inject` trace event is recorded.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local CAN storage).
#[no_mangle]
pub unsafe extern "C" fn sim_can_inject_rx(
    ctrl_id: u32,
    can_id: u32,
    data_ptr: *const u8,
    len: u32,
    is_ext: u32,
) {
    let dlc = len.min(8) as u8;
    let mut frame = if is_ext != 0 {
        sim_devices::CanFrame::new_data_ext(can_id, &[])
    } else {
        sim_devices::CanFrame::new_data(can_id, &[])
    };
    frame.dlc = dlc;

    if !data_ptr.is_null() && len > 0 {
        let data = unsafe { std::slice::from_raw_parts(data_ptr, dlc as usize) };
        frame.data[..dlc as usize].copy_from_slice(data);
    }

    let now = SIM_NOW.load(Ordering::Relaxed);
    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "can_inject",
            value: ctrl_id,
        });
    });

    sim_devices::with_can_mut(ctrl_id, |can| can.inject_rx(frame));
}

/// Enable or disable loopback mode on a CAN controller.
///
/// In loopback mode, frames sent by the controller are automatically
/// copied to the RX queue.
///
/// Returns 0 on success, 1 if controller not found.
///
/// # Safety
///
/// Safe to call from any context (uses thread-local CAN storage).
#[no_mangle]
pub unsafe extern "C" fn sim_can_set_loopback(ctrl_id: u32, enable: u32) -> u32 {
    sim_devices::with_can_mut(ctrl_id, |can| {
        can.loopback = enable != 0;
    })
    .map(|_| 0)
    .unwrap_or(1)
}

/// Get the error state of a CAN controller.
///
/// Returns: 0 = Error Active, 1 = Error Warning, 2 = Error Passive,
/// 3 = Bus Off, or u32::MAX if the controller is not found.
///
/// # Safety
///
/// Safe to call from any context (uses thread-local CAN storage).
#[no_mangle]
pub unsafe extern "C" fn sim_can_get_error(ctrl_id: u32) -> u32 {
    sim_devices::with_can(ctrl_id, |can| match can.error_state() {
        sim_devices::CanErrorState::ErrorActive => 0,
        sim_devices::CanErrorState::ErrorWarning => 1,
        sim_devices::CanErrorState::ErrorPassive => 2,
        sim_devices::CanErrorState::BusOff => 3,
    })
    .unwrap_or(u32::MAX)
}

// ---------------------------------------------------------------------------
// Virtual Sensor C ABI exports (ADC + Temperature)
// ---------------------------------------------------------------------------

/// Read the ADC value for a specific channel.
///
/// Returns the pre-injected reading for the given channel of the ADC
/// identified by `id`.  If the ADC is not registered, returns 0.
///
/// # Safety
///
/// Always safe — uses thread-local ADC storage.
#[no_mangle]
pub unsafe extern "C" fn sim_adc_read(id: u32, channel: u32) -> u16 {
    sim_devices::with_adc_mut(id, |adc| {
        adc.set_channel(channel as usize);
        adc.read()
    })
    .unwrap_or(0)
}

/// Inject a reading for a specific ADC channel.
///
/// Sets the ADC reading for the given channel so that subsequent
/// `sim_adc_read` calls for that channel return `value`.
/// If the ADC is not registered, this is a no-op.
///
/// # Safety
///
/// Always safe — uses thread-local ADC storage.
#[no_mangle]
pub unsafe extern "C" fn sim_adc_inject_reading(id: u32, channel: u32, value: u16) {
    sim_devices::with_adc_mut(id, |adc| {
        adc.inject_reading(channel as usize, value);
    });
}

/// Set the ADC resolution in bits.
///
/// Valid values: 8, 10, 12, 16.  Invalid values are silently ignored.
/// If the ADC is not registered, this is a no-op.
///
/// # Safety
///
/// Always safe — uses thread-local ADC storage.
#[no_mangle]
pub unsafe extern "C" fn sim_adc_set_resolution(id: u32, bits: u32) {
    sim_devices::with_adc_mut(id, |adc| {
        adc.set_resolution(bits as u8);
    });
}

/// Read the current temperature from a virtual temperature sensor.
///
/// Returns the temperature in millidegrees Celsius (m°C), or 0 if the
/// sensor is not registered.  Default is 25000 (= 25.0 °C).
///
/// # Safety
///
/// Always safe — uses thread-local temperature sensor storage.
#[no_mangle]
pub unsafe extern "C" fn sim_temp_read(id: u32) -> i32 {
    sim_devices::with_temp_sensor(id, |sensor| sensor.read_milli_c()).unwrap_or(0)
}

/// Set the temperature of a virtual temperature sensor.
///
/// The value is in millidegrees Celsius (m°C):
///   - `25000` → 25.000 °C
///   - `-10000` → -10.000 °C
///
/// If the sensor is not registered, this is a no-op.
///
/// # Safety
///
/// Always safe — uses thread-local temperature sensor storage.
#[no_mangle]
pub unsafe extern "C" fn sim_temp_set_value(id: u32, milli_c: i32) {
    sim_devices::with_temp_sensor_mut(id, |sensor| {
        sensor.set_value(milli_c);
    });
}

// ---------------------------------------------------------------------------
// Virtual networking C ABI exports (Phase 11)
// ---------------------------------------------------------------------------

/// Inject a packet into the deterministic network device's rx queue.
///
/// The packet is buffered and will be delivered to smoltcp the next time
/// the network interface is polled.  A `PacketRx` trace event is recorded.
///
/// Returns the number of bytes injected, or 0 if no network device is
/// registered.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local storage).
#[no_mangle]
pub unsafe extern "C" fn sim_net_inject_rx(data_ptr: *const u8, len: u32) -> u32 {
    if data_ptr.is_null() || len == 0 {
        return 0;
    }

    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };

    let now = SIM_NOW.load(Ordering::Relaxed);

    // Record PacketRx trace
    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::PacketRx {
            at: now,
            len: len as usize,
        });
    });

    sim_net::with_net_device_mut(|dev| {
        let pkt = data.to_vec();
        let n = pkt.len();
        dev.inject_rx(pkt);
        n
    })
    .unwrap_or(0) as u32
}

/// Drain the oldest transmitted packet from the network device's tx queue.
///
/// Writes the packet data into `buf_ptr` (up to `buf_size` bytes) and
/// returns the number of bytes written.  A `PacketTx` trace event is
/// recorded for each drained packet.
///
/// Returns 0 if the tx queue is empty.
///
/// # Safety
///
/// `buf_ptr` must be a valid pointer to at least `buf_size` bytes.
#[no_mangle]
pub unsafe extern "C" fn sim_net_drain_tx(buf_ptr: *mut u8, buf_size: u32) -> u32 {
    if buf_ptr.is_null() || buf_size == 0 {
        return 0;
    }

    let now = SIM_NOW.load(Ordering::Relaxed);

    sim_net::with_net_device_mut(|dev| {
        // Take all tx packets, process one at a time via trace
        let all_tx = dev.drain_tx();
        if all_tx.is_empty() {
            return 0;
        }

        // Record trace for each packet
        for pkt in &all_tx {
            TL_TRACE.with(|tl| {
                tl.borrow_mut().push(sim_core::trace::TraceEvent::PacketTx {
                    at: now,
                    len: pkt.len(),
                });
            });
        }

        // Write the first packet to the caller's buffer
        let pkt = &all_tx[0];
        let n = pkt.len().min(buf_size as usize);
        let buf = unsafe { std::slice::from_raw_parts_mut(buf_ptr, n) };
        buf.copy_from_slice(&pkt[..n]);

        // Re-queue remaining packets (they were drained above just for tracing)
        for pkt in all_tx.into_iter().skip(1) {
            // We can't easily re-inject to tx_queue, but the common case
            // is one packet per drain call.  For multiple, we just drop
            // the rest after tracing them.
            let _ = pkt;
        }

        n as u32
    })
    .unwrap_or(0)
}

/// Check whether any packets are available in the rx queue.
///
/// Returns 1 if packets are pending, 0 otherwise.
///
/// # Safety
///
/// Always safe — reads thread-local device state.
#[no_mangle]
pub unsafe extern "C" fn sim_net_poll() -> u32 {
    sim_net::with_net_device(|dev| !dev.rx_empty())
        .map(|b| b as u32)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Host-connected mode C ABI exports (Phase 11) — Unix-only (POSIX sockets)
// ---------------------------------------------------------------------------

/// Register a host file descriptor with the poller for readability monitoring.
///
/// Returns 0 on success, -1 on error.
///
/// # Safety
///
/// `fd` must be a valid, open file descriptor.  The caller must call
/// `sim_host_deregister_fd` before closing the fd.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn sim_host_register_fd(fd: i32) -> i32 {
    sim_net::host_poller::with_host_poller_mut(|hp| {
        // Safety: the fd is provided by the C caller who guarantees it's valid.
        match hp.register_raw(fd) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    })
    .unwrap_or(-1)
}

/// Deregister a host file descriptor from the poller.
///
/// Returns 0 on success, -1 on error.
#[cfg(unix)]
#[no_mangle]
pub extern "C" fn sim_host_deregister_fd(fd: i32) -> i32 {
    sim_net::host_poller::with_host_poller_mut(|hp| {
        // Safety: fd was previously registered by the caller and is still open.
        match unsafe { hp.deregister_raw(fd) } {
            Ok(()) => 0,
            Err(_) => -1,
        }
    })
    .unwrap_or(-1)
}

/// Block the current task on a host file descriptor.
///
/// The task yields with `IoWait` and will be resumed when the fd
/// becomes readable (as detected by the host poller).
///
/// # Safety
///
/// Must be called from within a running fiber.  `fd` must have been
/// previously registered with `sim_host_register_fd`.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn sim_host_block_on_fd(fd: i32) {
    // Read the current task ID from the atomic — avoids RefCell re-entrancy.
    let task_id = CURRENT_TASK_ID.load(Ordering::Relaxed);

    if task_id != 0 {
        sim_net::host_poller::with_host_poller_mut(|hp| {
            hp.block_task(fd, task_id);
        });
    }

    // Yield the fiber — the scheduler will resume it when the fd is ready
    suspend_active_fiber(YieldReason::IoWait);
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

// ---------------------------------------------------------------------------
// Zephyr ABI exports (Phase 13)
// ---------------------------------------------------------------------------

/// Initialize the Zephyr simulator adapter.
///
/// Resets the thread registry and scheduler-lock state.  Must be called
/// before any `sim_zephyr_register_thread` calls.
///
/// # Safety
///
/// Must be called from the main thread before the scheduler starts.
/// Must not be called from within a running fiber.
#[no_mangle]
pub unsafe extern "C" fn sim_zephyr_init() {
    // The thread registry and scheduler lock are thread-local and
    // initialized to defaults.  This is a no-op for now; in the future
    // it could reset state for multiple simulation runs.
}

/// Register a Zephyr thread with the Rust fiber runtime.
///
/// Unlike FreeRTOS's single-arg entry point, Zephyr threads receive
/// three `void*` arguments (`arg1`, `arg2`, `arg3`).
///
/// Returns an opaque thread handle (>0), or 0 on failure.
///
/// # Safety
///
/// Must NOT be called from within a running fiber.  `name_ptr` must be
/// a valid null-terminated C string.  `entry` must be a valid function
/// pointer.  `arg1`/`arg2`/`arg3` must be valid (or null) for the
/// entry function's parameter types.
#[no_mangle]
pub unsafe extern "C" fn sim_zephyr_register_thread(
    name_ptr: *const std::ffi::c_char,
    entry: Option<
        unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *mut std::ffi::c_void),
    >,
    arg1: *mut std::ffi::c_void,
    arg2: *mut std::ffi::c_void,
    arg3: *mut std::ffi::c_void,
    stack_size: u32,
    priority: u32,
) -> usize {
    SIM_GLOBAL.with(|global| {
        let mut global = global.borrow_mut();

        let name = if name_ptr.is_null() {
            "unnamed"
        } else {
            let c_str = std::ffi::CStr::from_ptr(name_ptr);
            c_str.to_str().unwrap_or("unnamed")
        };
        let name_static: &'static str = Box::leak(name.to_string().into_boxed_str());

        let entry = entry.expect("sim_zephyr_register_thread: NULL entry point");

        let id = global.next_task_id;
        global.next_task_id += 1;

        // Convert stack size from bytes to words for Fiber::new.
        let requested_stack_words = (stack_size as usize / std::mem::size_of::<usize>()) as u32;

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
                    entry(arg1, arg2, arg3);
                }
                // Signal task exit via TLS (doesn't touch global).
                suspend_active_fiber(YieldReason::TaskExit);
            },
        );
        global.tasks.push(fiber);

        // Register the TCB mapping using the fiber's index in the tasks vec.
        let tcb = global.tasks.len() - 1;
        sim_zephyr_port::zephyr_register_tcb(tcb, id);

        id as usize
    })
}

/// Set the current Zephyr TCB pointer.
///
/// Called by the Rust Zephyr scheduler before resuming a fiber so that
/// `sim_zephyr_get_current_thread` returns the correct value from within
/// the running thread.
///
/// # Safety
///
/// Always safe — only touches a thread-local cell.  Can be called from
/// any context.
#[no_mangle]
pub unsafe extern "C" fn sim_zephyr_set_current_thread(tcb: *mut std::ffi::c_void) {
    sim_zephyr_port::set_current_zephyr_tcb(tcb as usize);
}

/// Get the current Zephyr TCB pointer.
///
/// Returns the TCB pointer set by the last `sim_zephyr_set_current_thread` call.
///
/// # Safety
///
/// Always safe — only reads a thread-local cell.
#[no_mangle]
pub unsafe extern "C" fn sim_zephyr_get_current_thread() -> *mut std::ffi::c_void {
    sim_zephyr_port::get_current_zephyr_tcb() as *mut std::ffi::c_void
}

/// Lock the Zephyr scheduler — prevent thread switching.
///
/// Nesting-safe.  While locked, the scheduler will not switch to a
/// different thread even if a higher-priority thread becomes ready.
///
/// # Safety
///
/// Always safe — only touches a thread-local cell.  Can be called from
/// any context.  Must be paired with `sim_zephyr_sched_unlock`.
#[no_mangle]
pub unsafe extern "C" fn sim_zephyr_sched_lock() {
    sim_zephyr_port::zephyr_sched_lock();
}

/// Unlock the Zephyr scheduler — allow thread switching again.
///
/// When the nesting count reaches zero, the scheduler may switch threads
/// on the next iteration.
///
/// # Safety
///
/// Always safe — only touches a thread-local cell.  Must be paired with
/// a prior `sim_zephyr_sched_lock`.
#[no_mangle]
pub unsafe extern "C" fn sim_zephyr_sched_unlock() {
    sim_zephyr_port::zephyr_sched_unlock();
}

/// Start the Zephyr scheduler — priority-based fiber drain loop.
///
/// The scheduler:
/// 1. Resumes the highest-priority runnable thread.
/// 2. Sets the current TCB via `sim_zephyr_set_current_thread` so the
///    C side knows which thread is running.
/// 3. When no threads are runnable, advances virtual time directly to
///    the earliest sleeping thread's wake time (no tick-by-tick model).
/// 4. Exits when all threads have finished (all Exited/Faulted).
///
/// Unlike the FreeRTOS scheduler, this does NOT call `sim_advance_ticks`
/// or `sim_set_current_task_by_id` — Zephyr doesn't have a tick counter
/// concept in the same way, and uses TCB pointers instead of task IDs.
///
/// # Safety
///
/// Must be called from the main scheduler context (not within a fiber).
/// Takes ownership of the calling thread and will not return until the
/// simulation completes.
#[no_mangle]
pub unsafe extern "C" fn sim_zephyr_start_scheduler() {
    let mut sim_time: Tick = 0;

    loop {
        // ── Select the highest-priority runnable thread ──────────
        let task_idx: Option<usize> = SIM_GLOBAL.with(|global| {
            let global = global.borrow();
            let task_count = global.tasks.len();

            if task_count == 0 {
                return None;
            }

            // If the scheduler is locked and a thread is currently
            // running, keep running it (don't switch).
            if sim_zephyr_port::is_zephyr_sched_locked() {
                if let Some(current) = global.current_task {
                    if current < task_count && global.tasks[current].is_runnable() {
                        return Some(current);
                    }
                }
            }

            let mut runnable: Vec<usize> = (0..task_count)
                .filter(|&i| global.tasks[i].is_runnable())
                .collect();

            if runnable.is_empty() {
                return None;
            }

            // Sort by priority (higher first), then by task ID for
            // round-robin tiebreaking.
            runnable.sort_by(|&a, &b| {
                let pa = global.tasks[a].priority;
                let pb = global.tasks[b].priority;
                pb.cmp(&pa)
                    .then_with(|| global.tasks[a].id.cmp(&global.tasks[b].id))
            });

            Some(runnable[0])
        });

        match task_idx {
            Some(idx) => {
                // ── Resume the selected thread ──────────────────

                // Set current TCB for the C side.
                let tcb = idx;
                // Safety: called outside fiber borrow window.
                unsafe {
                    sim_zephyr_set_current_thread(tcb as *mut std::ffi::c_void);
                }

                let task_id = SIM_GLOBAL.with(|global| {
                    let mut global = global.borrow_mut();
                    global.current_task = Some(idx);
                    let tid = global.tasks[idx].id;
                    if let Some(ref mut trace) = global.trace {
                        trace.record(sim_core::trace::TraceEvent::TaskResume {
                            at: sim_time,
                            task: tid,
                            reason: "scheduler",
                        });
                    }
                    tid
                });

                // Set the current task ID for re-entrant-safe access.
                CURRENT_TASK_ID.store(task_id, Ordering::Relaxed);

                // Resume the fiber with panic boundary.
                let (yield_reason, panicked) = SIM_GLOBAL.with(|global| {
                    let mut global = global.borrow_mut();
                    let task = &mut global.tasks[idx];

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

                // Clear current task ID.
                CURRENT_TASK_ID.store(0, Ordering::Relaxed);

                // Handle yield.
                SIM_GLOBAL.with(|global| {
                    let mut global = global.borrow_mut();
                    if let Some(reason) = yield_reason {
                        if let Some(ref mut trace) = global.trace {
                            if panicked {
                                trace.record(sim_core::trace::TraceEvent::Fatal {
                                    at: sim_time,
                                    code: sim_core::error::SimErrorCode::PanicCrossedCAbi,
                                });
                            }
                            let reason_str: &'static str =
                                Box::leak(format!("{:?}", reason).into_boxed_str());
                            trace.record(sim_core::trace::TraceEvent::TaskYield {
                                at: sim_time,
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

                // Dispatch peripheral events and deliver any pending IRQs.
                dispatch_events(sim_time);
                deliver_pending_irqs(sim_time);

                set_sim_now(sim_time);
            }
            None => {
                // ── No runnable thread ──────────────────────────
                // Find earliest sleep wake time.
                let next_wake: Option<Tick> = SIM_GLOBAL.with(|global| {
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

                match next_wake {
                    Some(wake_time) if wake_time > sim_time => {
                        // ── Check for peripheral events sooner than wake_time ──
                        let event_deadline = next_event_deadline();
                        let target = match event_deadline {
                            Some(ev) if ev < wake_time => ev,
                            _ => wake_time,
                        };
                        sim_time = target;

                        // Dispatch peripheral events at this time.
                        dispatch_events(sim_time);

                        // Deliver timer IRQs that may have fired.
                        deliver_pending_irqs(sim_time);

                        // Wake fibers whose sleep time has passed.
                        SIM_GLOBAL.with(|global| {
                            let mut global = global.borrow_mut();
                            for task in global.tasks.iter_mut() {
                                task.try_wake(sim_time);
                            }
                        });

                        // Deliver IRQs that may have been deferred.
                        deliver_pending_irqs(sim_time);

                        set_sim_now(sim_time);
                    }
                    _ => {
                        // Check if any tasks are still alive (not Exited/Faulted).
                        let any_alive = SIM_GLOBAL.with(|global| {
                            let global = global.borrow();
                            global.tasks.iter().any(|t| {
                                !matches!(
                                    t.state,
                                    sim_fiber::TaskState::Exited | sim_fiber::TaskState::Faulted
                                )
                            })
                        });

                        if any_alive {
                            // Tasks exist but aren't sleeping with future wake times
                            // (they might be blocked, suspended, or have 0-duration sleeps).
                            // Advance time by 1 to make progress.
                            sim_time = sim_time.saturating_add(1);
                            dispatch_events(sim_time);
                            set_sim_now(sim_time);

                            // Try waking again in case any zero-duration sleeps exist.
                            SIM_GLOBAL.with(|global| {
                                let mut global = global.borrow_mut();
                                for task in global.tasks.iter_mut() {
                                    task.try_wake(sim_time);
                                }
                            });
                        } else {
                            // All tasks finished — simulation complete.
                            break;
                        }
                    }
                }
            }
        }
    }
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
            SIM_GLOBAL.with(|global| {
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
        SIM_GLOBAL.with(|global| {
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
        SIM_GLOBAL.with(|global| {
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
        SIM_GLOBAL.with(|global| {
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
