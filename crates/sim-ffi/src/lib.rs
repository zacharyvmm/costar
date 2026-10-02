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
//!   - `sim_enter_critical` / `sim_exit_critical` → the machine's interrupt state
//!   - `sim_trace_u32` → append to a thread-local trace buffer

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::AtomicU64;

use sim_core::time::Tick;
use sim_core::trace::{TraceEvent, TraceSink};
use sim_fiber::yield_reason::YieldReason;
use sim_fiber::{has_active_fiber, suspend_active_fiber, Fiber, TaskId};

pub mod device_ffi;
pub mod freertos;
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
    fn sim_freertos_context_create() -> *mut std::ffi::c_void;
    fn sim_freertos_context_activate(context: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn sim_freertos_context_destroy(context: *mut std::ffi::c_void);
    /// Single-tick advance. C code calls this via sim_advance_ticks(1)
    /// rather than directly, so Rust never invokes it. The extern
    /// declaration is kept for completeness and as documentation of
    /// the C ABI surface.
    #[allow(dead_code)]
    fn sim_tick_advance() -> u32;
    fn sim_advance_ticks(count: u32) -> u32;
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
    /// FreeRTOS tick wrapper state.  This belongs to the active Simulator,
    /// not the host thread: Worlds can interleave on one thread.
    pub scheduler_initialized: bool,
    pub scheduler_sim_time: Tick,
    /// Set once a FreeRTOS task exists: from then on FreeRTOS makes every
    /// scheduling decision (see [`freertos`]).
    pub freertos: bool,
    /// FreeRTOS tasks whose fiber has not been claimed by a legacy
    /// `sim_create_task()` call, as `(task id, entry address)`.
    pub(crate) unclaimed_freertos_tasks: Vec<UnclaimedTask>,
    /// FreeRTOS: the last scheduling step found nothing that can ever run
    /// again without external input.
    pub freertos_quiescent: bool,
    /// FreeRTOS: `vTaskEndScheduler()` was called.
    pub freertos_ended: bool,
    /// Latest FreeRTOS tick the scheduler may advance to in one
    /// [`sim_scheduler_tick`] call.  Set by a World from its own clock so
    /// firmware time never runs ahead of the World.  `None` = one
    /// scheduling step per call, no limit.
    pub scheduler_limit: Option<Tick>,
    /// FreeRTOS: tick at which the scheduler next needs to run (a delayed
    /// task, a timer or a peripheral event), as reported by the last
    /// [`sim_scheduler_tick`] call.  `None` = only external input can wake it.
    pub freertos_next_wake: Option<Tick>,
    /// FreeRTOS: the idle task is parked at `scheduler_limit`.
    pub(crate) freertos_parked: bool,
    /// FreeRTOS: a task used up its budget at `scheduler_limit`; the tick
    /// interrupt that stands for is charged at the start of the next step,
    /// before any task runs (as standalone stepping charges it at once).
    pub(crate) freertos_tick_owed: bool,
    /// FreeRTOS: ticks of virtual time that passed while interrupts were
    /// masked.  The tick interrupt is masked too, so the kernel has not
    /// counted them yet: they are serviced (`xTaskIncrementTick()`) as soon
    /// as interrupts are unmasked, like a pending SysTick.
    pub(crate) freertos_masked_ticks: u64,
    /// FreeRTOS: the selected task may not run before interrupts are
    /// unmasked (see `freertos::held_by_mask`).
    pub(crate) freertos_held_by_mask: bool,
    /// The machine was stopped by a fatal error (an interrupt storm, an
    /// unrecoverable kernel state): guest-facing calls are no-ops.
    pub(crate) fatal_stop: bool,
    /// `(tick, count)` of work units at `tick` that made no time progress
    /// (peripheral callbacks, deadlines due again at the same tick).  See
    /// [`freertos::no_progress_at`].
    pub(crate) no_progress: (Tick, u32),
    /// Work units one tick may take without time moving before the machine
    /// is stopped as an interrupt storm ([`freertos::storm_fatal`]).
    pub(crate) storm_limit: u32,
    /// FreeRTOS tasks suspended in the kernel until the host poller reports
    /// their descriptor ready, as `(task id, TCB address)`.
    pub(crate) freertos_io_waits: Vec<(TaskId, usize)>,
    /// Native tasks FreeRTOS does not schedule yet, as `(task id, origin)`:
    /// Rust tasks from [`spawn_rust_task`] (`origin` = `None`) and tasks
    /// created directly with [`sim_create_task`] that no FreeRTOS task
    /// claimed (`origin` = their C entry point and parameter).  Once the machine runs
    /// FreeRTOS, the engine gives each one a FreeRTOS task of its own (see
    /// [`freertos::adopt_native_tasks`]).
    pub(crate) native_tasks_to_adopt: Vec<(TaskId, Option<(usize, usize)>)>,
    /// Tasks whose host descriptor the poller reported readable while they
    /// waited in `sim_host_block_on_fd()`, until the task consumes it
    /// ([`take_io_ready`]).  Latched here because the poller forgets the
    /// readiness and the task association when it reports them.
    pub(crate) io_ready: Vec<TaskId>,
    /// Tasks whose host I/O wait was ended without readiness (its
    /// descriptor was deregistered, see [`end_io_wait`]), until the task
    /// consumes it ([`take_io_cancelled`]).  Latched per wait, so
    /// re-registering the descriptor before the task resumes does not
    /// revive the wait.
    pub(crate) io_cancelled: Vec<TaskId>,
}

/// A FreeRTOS task no legacy `sim_create_task()` call has claimed yet.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UnclaimedTask {
    pub(crate) id: TaskId,
    pub(crate) entry: usize,
    pub(crate) arg: usize,
    /// The TCB's (possibly truncated) task name.
    pub(crate) name: &'static str,
    /// The TCB's priority.
    pub(crate) priority: u32,
}

impl SimGlobal {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            current_task: None,
            interrupt: InterruptState::default(),
            next_task_id: 1,
            trace: None,
            scheduler_initialized: false,
            scheduler_sim_time: 0,
            freertos: false,
            unclaimed_freertos_tasks: Vec::new(),
            freertos_quiescent: false,
            freertos_ended: false,
            scheduler_limit: None,
            freertos_next_wake: None,
            freertos_parked: false,
            freertos_tick_owed: false,
            freertos_masked_ticks: 0,
            freertos_held_by_mask: false,
            fatal_stop: false,
            no_progress: (0, 0),
            storm_limit: freertos::DEFAULT_STORM_LIMIT,
            freertos_io_waits: Vec::new(),
            native_tasks_to_adopt: Vec::new(),
            io_ready: Vec::new(),
            io_cancelled: Vec::new(),
        }
    }

    /// Earliest `Sleeping { until }` deadline among tasks, if any.
    ///
    /// FreeRTOS tasks sleep on FreeRTOS's own delayed list, which the
    /// scheduler advances to by itself; this only covers native fibers.
    pub fn earliest_sleep_until(&self) -> Option<Tick> {
        if self.freertos {
            return None;
        }
        self.tasks
            .iter()
            .filter_map(|t| match t.state {
                sim_fiber::TaskState::Sleeping { until } => Some(until),
                _ => None,
            })
            .min()
    }

    /// A task was created or made ready.  On a FreeRTOS machine, the result
    /// of the last scheduling step (quiescent, next wake-up) no longer holds:
    /// the task is ready now.  Inside a step this is overwritten by the step's
    /// own report; between steps (e.g. `Firmware::step` spawning a task
    /// after running the scheduler) it makes the machine run again at once.
    pub(crate) fn note_new_task(&mut self) {
        // An ended machine never runs again (see `sim_scheduler_tick`).
        if self.freertos && !self.freertos_ended {
            self.freertos_quiescent = false;
            let now = self.scheduler_sim_time;
            self.freertos_next_wake = Some(self.freertos_next_wake.map_or(now, |w| w.min(now)));
        }
    }

    /// True when any task can run without waiting for time to advance.
    pub fn has_runnable_task(&self) -> bool {
        if self.freertos {
            return !self.freertos_quiescent;
        }
        self.tasks.iter().any(|t| t.is_runnable())
    }
}

impl Default for SimGlobal {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    static SIM_GLOBAL: Rc<RefCell<SimGlobal>> = Rc::new(RefCell::new(SimGlobal::new()));

    /// Active simulator contexts in activation order.  Each entry owns the
    /// selected `SimGlobal`, so lookup never relies on a borrowed raw pointer.
    static ACTIVE_SIM_GLOBALS: RefCell<Vec<Rc<ActiveSimGlobal>>> =
        const { RefCell::new(Vec::new()) };
}

// ---------------------------------------------------------------------------
// Per-Simulator active context
// ---------------------------------------------------------------------------

struct ActiveSimGlobal {
    global: Rc<RefCell<SimGlobal>>,
}

/// Access the current `SimGlobal` — either the active simulator's or the
/// thread-local fallback.
///
/// This is the single access point used by all C ABI functions.  The selected
/// handle is cloned before `f` runs, so arbitrary C ABI work cannot retain a
/// borrow of the thread-local activation stack and the selected global remains
/// alive for the full callback.
#[inline]
pub(crate) fn with_sim_global<F, R>(f: F) -> R
where
    F: FnOnce(&RefCell<SimGlobal>) -> R,
{
    let active_global = ACTIVE_SIM_GLOBALS.with(|active| {
        active
            .borrow()
            .last()
            .map(|activation| activation.global.clone())
    });

    if let Some(global) = active_global {
        f(&global)
    } else {
        SIM_GLOBAL.with(|global| f(global))
    }
}

/// Whether a [`Simulator`](crate::simulator::Simulator) is active on this
/// thread (as opposed to the standalone thread-local fallback state).
pub(crate) fn has_active_simulator() -> bool {
    ACTIVE_SIM_GLOBALS.with(|active| !active.borrow().is_empty())
}

/// Activate a `SimGlobal` for the current thread — C ABI calls will
/// operate on this state until deactivated.
///
/// The returned guard owns an activation-stack entry that retains the selected
/// global.  This remains memory-safe even if guards are dropped out of order
/// or deliberately forgotten; callers should nevertheless prefer the
/// closure-scoped `SimulatorExecutionContext::with_active` API.
pub(crate) fn activate_sim_global(sim_global: &Rc<RefCell<SimGlobal>>) -> SimGlobalGuard {
    let activation = Rc::new(ActiveSimGlobal {
        global: sim_global.clone(),
    });
    ACTIVE_SIM_GLOBALS.with(|active| active.borrow_mut().push(activation.clone()));
    SimGlobalGuard { activation }
}

/// Opaque guard that removes its own `SimGlobal` activation on drop.
#[must_use = "an active simulator context ends when its guard is dropped"]
pub(crate) struct SimGlobalGuard {
    activation: Rc<ActiveSimGlobal>,
}

impl Drop for SimGlobalGuard {
    fn drop(&mut self) {
        // Removing by identity preserves the currently active inner context
        // when an outer guard is dropped first.  Avoid panicking during TLS
        // teardown if a guard outlives the thread-local stack.
        let _ = ACTIVE_SIM_GLOBALS.try_with(|active| {
            let mut active = active.borrow_mut();
            if let Some(index) = active
                .iter()
                .rposition(|entry| Rc::ptr_eq(entry, &self.activation))
            {
                active.remove(index);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// C ABI exports
// ---------------------------------------------------------------------------

/// Return the current virtual time in ticks.
///
/// # Safety
///
/// Always safe — reads from the active GuestRuntime's Cell when available,
/// falling back to the atomic.  Can be called from any context.
#[no_mangle]
pub unsafe extern "C" fn sim_now_ticks() -> u64 {
    guest_runtime::active_now()
}

/// Set the current virtual time (called from the scheduler only).
pub fn set_sim_now(now: Tick) {
    guest_runtime::set_active_now(now);
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
        let name_static: &'static str = sim_core::trace::intern(name);

        let entry = entry.expect("sim_create_task: NULL entry point");

        // Legacy firmware pattern: `xTaskCreate()` + `sim_create_task()` +
        // `sim_bridge_register()` for the same task.  `xTaskCreate()` has
        // already created the task's fiber, so return that handle instead of
        // creating a second fiber FreeRTOS would never schedule.
        if global.freertos {
            // Only a live task can be the other half of the pair: one that
            // already ran to completion (or was deleted) is gone, and a new
            // `sim_create_task()` for its entry is a new task.
            let deleting = PENDING_DELETIONS.with(|pd| pd.borrow().clone());
            let SimGlobal {
                unclaimed_freertos_tasks: unclaimed,
                tasks,
                ..
            } = &mut *global;
            unclaimed.retain(|u| {
                !deleting.contains(&u.id)
                    && tasks.iter().any(|t| t.id == u.id && !t.is_terminated())
            });
            // The pair shares entry, parameter, name (FreeRTOS keeps at
            // most configMAX_TASK_NAME_LEN - 1 bytes) and priority; calls
            // that differ in any are independent tasks.
            let pos = unclaimed.iter().position(|u: &UnclaimedTask| {
                u.entry == entry as usize
                    && u.arg == arg as usize
                    && freertos::legacy_pair_matches(name, priority, u.name, u.priority)
            });
            if let Some(pos) = pos {
                return unclaimed.remove(pos).id as usize;
            }
        }

        let id = global.next_task_id;
        global.next_task_id += 1;

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
                if freertos::schedules_native_task() {
                    // Remove the task from FreeRTOS; never resumed after.
                    freertos::delete_current_task();
                }
                // Signal task exit via TLS (doesn't touch global).
                loop {
                    suspend_active_fiber(YieldReason::TaskExit);
                }
            },
        );
        global.tasks.push(fiber);
        // On a FreeRTOS machine the task gets a FreeRTOS task of its own
        // (or the TCB of a matching `xTaskCreate()` that follows, the
        // legacy pattern in reverse order), so FreeRTOS schedules it.
        global
            .native_tasks_to_adopt
            .push((id, Some((entry as usize, arg as usize))));
        global.note_new_task();

        // Emit a TaskCreated trace event so symbolication tools can
        // resolve task IDs to names.
        if let Some(ref mut trace) = global.trace {
            trace.record(TraceEvent::TaskCreated {
                at: guest_runtime::active_now(),
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
    let name_static: &'static str = sim_core::trace::intern(name);

    with_sim_global(|global| {
        let mut global = global.borrow_mut();
        if let Some(ref mut trace) = global.trace {
            trace.record(TraceEvent::TaskCreated {
                at: guest_runtime::active_now(),
                task: task_id,
                name: name_static,
            });
        }
    });
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
    // A stopped machine (an interrupt storm, a fatal kernel state) is done.
    if freertos::halted() {
        return false;
    }
    if with_sim_global(|global| global.borrow().freertos) {
        return freertos::cycle(sim_time);
    }

    // IRQ input that has arrived (host input staged with `raise_at` for
    // the current tick, an expired timer) is taken before any task is
    // selected or resumed, as on FreeRTOS: a task continues only after the
    // ISR, never on stale device state.
    if !is_critical_locked() && sim_devices::irq::with_irq(|c| c.first_due(*sim_time).is_some()) {
        set_sim_now(*sim_time);
        deliver_pending_irqs(*sim_time);
        if freertos::halted() {
            return false;
        }
    }

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

            // Tell C which TCB is current (legacy table; FreeRTOS tasks are
            // scheduled by `freertos::cycle`, never through this path).
            let task_id = with_sim_global(|global| global.borrow().tasks[idx].id);
            // Safety: called from scheduler context, outside any fiber.
            unsafe {
                sim_set_current_task_by_id(task_id);
            }
            let reason = resume_task(idx, *sim_time);
            // Process any task deletions recorded during the slice, then
            // release the interrupt state of a task retired in it (also
            // one that exited from inside an ISR it was running), once,
            // before an ISR can set new state.
            process_pending_deletions();
            release_state_of_stopped_fiber(idx, reason);

            // Deliver any pending IRQs and expired timers.
            deliver_pending_irqs(*sim_time);

            // Process any task deletions recorded during the
            // fiber's execution (vTaskDelete from C code).
            process_pending_deletions();

            // Bridge Ethernet loopback: deliver guest-sent frames
            // back to the receive queue so receiver tasks can read them.
            eth_loopback_bridge();

            set_sim_now(*sim_time);

            // Work was done; continue, unless the slice (a storm started
            // by the task's IRQ) or the delivery after it stopped the
            // machine.
            !freertos::halted()
        }
        None => {
            // ── No runnable task ──────────────────────────

            // Process any pending task deletions first.
            process_pending_deletions();

            // Bridge Ethernet loopback in the idle path too.
            eth_loopback_bridge();

            // ── Deadlines ─────────────────────────────────
            // The next sleeper wake-up, peripheral callback and scheduled
            // IRQ arrival (`IrqController::raise_at`, a World's
            // `Machine::raise_irq`).  One deadline per cycle: time moves to
            // the earliest, what is due there runs, and the next cycle
            // reconsiders (a callback or an ISR may have added earlier
            // work).  Never past a World step's limit (`scheduler_limit`):
            // firmware time does not run ahead of the World, and input is
            // not taken before the World reaches it.  IRQ input already due
            // is taken now (unless interrupts are masked): that is input the
            // mask held off, at the World's current time if nothing else is
            // due before it.
            let limit = with_sim_global(|global| global.borrow().scheduler_limit);
            let irq_arrival = sim_devices::irq::with_irq(|c| c.next_arrival_after(*sim_time));
            let target = [
                next_wake.map(|wake| wake.max(*sim_time)),
                event_target(*sim_time),
                irq_arrival,
            ]
            .into_iter()
            .flatten()
            .min();
            if !is_critical_locked()
                && sim_devices::irq::with_irq(|c| c.first_due(*sim_time).is_some())
            {
                if target.is_none_or(|at| limit.is_some_and(|limit| at > limit)) {
                    catch_up_to_limit(sim_time, limit);
                }
                set_sim_now(*sim_time);
                deliver_pending_irqs(*sim_time);
                return !freertos::halted();
            }

            let io_waiting_now = || {
                with_sim_global(|global| {
                    global
                        .borrow()
                        .tasks
                        .iter()
                        .any(|t| matches!(t.state, sim_fiber::TaskState::IoWaiting))
                })
            };
            // Host I/O waiters whose descriptors are already ready run
            // before time moves: a chain of callbacks (each scheduling the
            // next) must not starve them.  A non-blocking poll.
            if target.is_some()
                && io_waiting_now()
                && host_poll_and_wake(*sim_time, Some(*sim_time)) > 0
            {
                deliver_pending_irqs(*sim_time);
                set_sim_now(*sim_time);
                return !freertos::halted();
            }

            match target {
                Some(at) if limit.is_none_or(|limit| at <= limit) => {
                    debug_assert!(at >= *sim_time, "virtual time ran backwards");
                    // Tickless idle: batch-advance the ticks in one C↔Rust
                    // crossing.
                    let ticks_to_advance = at - *sim_time;
                    *sim_time = at;
                    // The new time is published before anything runs at
                    // it: callbacks and ISRs read it.
                    set_sim_now(*sim_time);
                    if ticks_to_advance > 0 {
                        unsafe {
                            sim_advance_ticks(ticks_to_advance as u32);
                        }
                    }
                    dispatch_events(*sim_time);
                    // A callback storm stopped the machine: never wake the
                    // sleepers.
                    if freertos::halted() {
                        return false;
                    }

                    // Wake fibers whose sleep time has passed.
                    with_sim_global(|global| {
                        let mut global = global.borrow_mut();
                        for task in global.tasks.iter_mut() {
                            task.try_wake(*sim_time);
                        }
                    });

                    // Timer expiries and IRQ input due now.
                    deliver_pending_irqs(*sim_time);

                    // Also poll host FDs, waiting no longer than the next
                    // sleeper's wake-up.
                    if io_waiting_now() {
                        let next_wake_after =
                            with_sim_global(|global| global.borrow().earliest_sleep_until());
                        host_poll_and_wake(*sim_time, next_wake_after);
                        deliver_pending_irqs(*sim_time);
                    }

                    set_sim_now(*sim_time);

                    // Time advanced; continue, unless what ran at the new
                    // time stopped the machine.
                    !freertos::halted()
                }
                // The next deadline is past the World step's limit: nothing
                // can happen before the World gets there, which wakes the
                // machine for it.  Firmware time keeps in step with the
                // World.
                Some(_) => {
                    catch_up_to_limit(sim_time, limit);
                    false
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

                    // No sleeping tasks and no I/O progress — simulation
                    // complete until input arrives.  Firmware time keeps in
                    // step with the World.
                    catch_up_to_limit(sim_time, limit);
                    false
                }
            }
        }
    }
}

/// The task at `idx` was retired in the slice it just ran: it finished,
/// exited or faulted (`reason`), or was deleted (also when deleting itself
/// was cut short inside the kernel's critical section, e.g. by a budget
/// tick).  The interrupt state it held — a critical section, a mask —
/// belongs to the task, as a port saves the critical nesting per task, and
/// dies with it, so the next task or ISR starts from its own state.  That
/// includes an ISR the task was running when it stopped (an ISR calling
/// `sim_task_exit()`).  Every backend's scheduler calls this once per
/// slice, right after it and before anything else (an ISR) can set new
/// interrupt state.  Returns whether the task was retired.
pub(crate) fn release_state_of_stopped_fiber(idx: usize, reason: Option<YieldReason>) -> bool {
    let stopped =
        matches!(
            reason,
            Some(YieldReason::Fault) | Some(YieldReason::TaskExit) | None
        ) || with_sim_global(|g| g.borrow().tasks.get(idx).is_some_and(|t| t.is_terminated()));
    if stopped {
        guest_runtime::update_interrupt_state(|s| *s = Default::default());
    }
    stopped
}

/// An idle native machine with nothing due by a World step's `limit`:
/// move firmware time to the limit, so it stays in step with the World
/// (and input the World stages for later maps onto the same clock).
fn catch_up_to_limit(sim_time: &mut Tick, limit: Option<Tick>) {
    if let Some(limit) = limit.filter(|&limit| limit > *sim_time) {
        let ticks = limit - *sim_time;
        *sim_time = limit;
        set_sim_now(limit);
        // Safety: scheduler context.
        unsafe { sim_advance_ticks(ticks as u32) };
    }
}

/// Between steps, bring an idle native machine's clock up to its World
/// step's limit if nothing (a runnable task, a sleeper, a callback, IRQ
/// input) is due before it: only time moves, no guest code runs.  Host
/// code acting on the firmware before the scheduler runs in the step (say,
/// unmasking interrupts, which delivers the IRQs held off) then acts at the
/// World's current time.  See `Simulator::catch_up_to_limit`.
pub(crate) fn native_catch_up_to_limit() {
    let (initialized, mut sim_time, limit, next_wake, runnable) = with_sim_global(|g| {
        let g = g.borrow();
        (
            g.scheduler_initialized,
            g.scheduler_sim_time,
            g.scheduler_limit,
            g.earliest_sleep_until(),
            g.has_runnable_task(),
        )
    });
    let Some(limit) = limit.filter(|&limit| initialized && !runnable && limit > sim_time) else {
        return;
    };
    let due_by_limit = [
        next_wake,
        event_target(sim_time),
        sim_devices::irq::with_irq(|c| c.next_arrival_after(sim_time)),
    ]
    .into_iter()
    .flatten()
    .any(|at| at <= limit);
    if due_by_limit {
        return;
    }
    catch_up_to_limit(&mut sim_time, Some(limit));
    with_sim_global(|g| g.borrow_mut().scheduler_sim_time = sim_time);
}

/// Resume the fiber at `idx` until it yields, and record the slice in the
/// trace.  Returns the yield reason (`None` if the fiber had already ended).
///
/// The fiber is moved out of the task table while it runs, so code inside
/// the task may freely call back into the engine — e.g. `xTaskCreate()`
/// from a running task creates a new fiber.
pub(crate) fn resume_task(idx: usize, sim_time: Tick) -> Option<YieldReason> {
    // A stopped machine (an interrupt storm, a fatal kernel state) runs no
    // guest code, whichever scheduler path got here.
    if freertos::halted() {
        return None;
    }
    let (task_id, mut fiber) = with_sim_global(|global| {
        let mut global = global.borrow_mut();
        let tid = global.tasks[idx].id;
        global.current_task = Some(idx);
        if let Some(ref mut trace) = global.trace {
            trace.record(TraceEvent::TaskResume {
                at: sim_time,
                task: tid,
                reason: "scheduler",
            });
        }
        (tid, global.tasks[idx].take_for_resume())
    });

    // Set the current task ID for re-entrant-safe access from within the
    // fiber (e.g., sim_host_block_on_fd).
    guest_runtime::set_active_task_id(task_id);

    // Resume the fiber, catching panics so a single misbehaving task does
    // not crash the entire simulator process.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fiber.resume(sim_fiber::ResumeReason::SchedulerSelected)
    }));
    let (yield_reason, panicked) = match result {
        Ok(reason) => (reason, false),
        Err(_panic_payload) => {
            fiber.state = sim_fiber::TaskState::Faulted;
            (Some(YieldReason::Fault), true)
        }
    };

    guest_runtime::set_active_task_id(0);

    with_sim_global(|global| {
        let mut global = global.borrow_mut();
        global.tasks[idx].restore(fiber);
        if let Some(reason) = yield_reason {
            if let Some(ref mut trace) = global.trace {
                if panicked {
                    trace.record(TraceEvent::Fatal {
                        at: sim_time,
                        code: sim_core::error::SimErrorCode::PanicCrossedCAbi,
                    });
                }
                trace.record(TraceEvent::TaskYield {
                    at: sim_time,
                    task: task_id,
                    reason: reason.trace_cause(),
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

    retire_registrations_if_stopped(task_id, yield_reason);
    yield_reason
}

/// A task whose fiber stopped for good in the slice it just ran (it
/// faulted — a panic, a failed `configASSERT()` —, exited or finished)
/// leaves nothing behind in the engine: every registration keyed by the
/// task is dropped (see [`freertos::cancel_io_wait`]), so nothing it
/// registered keeps the machine running or revives it.  Deletion does the
/// same from `traceTASK_DELETE`.  Every backend's scheduler calls this
/// after each slice.
pub(crate) fn retire_registrations_if_stopped(task: TaskId, reason: Option<YieldReason>) {
    if matches!(
        reason,
        Some(YieldReason::Fault) | Some(YieldReason::TaskExit) | None
    ) {
        freertos::cancel_io_wait(task);
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
///
/// It is the same scheduler as [`sim_scheduler_tick`], not a second one:
/// it starts FreeRTOS if needed, continues from the machine's current
/// virtual time (e.g. after native-only steps) and keeps the scheduler
/// state for later steps.  It runs unbounded: a
/// [`scheduler_limit`](SimGlobal::scheduler_limit) is ignored meanwhile
/// and restored afterwards.  `vTaskStartScheduler()` in standalone firmware
/// ends up here.
#[no_mangle]
pub unsafe extern "C" fn sim_start_scheduler() {
    let limit = with_sim_global(|g| g.borrow_mut().scheduler_limit.take());
    while sim_scheduler_tick() != 0 {}
    with_sim_global(|g| g.borrow_mut().scheduler_limit = limit);
}

// ---------------------------------------------------------------------------
// C ABI: sim_scheduler_tick (single-cycle advancement)
// ---------------------------------------------------------------------------

/// Advance the FreeRTOS scheduler and return.
///
/// The first call starts FreeRTOS (idle and timer tasks) if the firmware
/// created tasks or timers but did not call `vTaskStartScheduler()`.
///
/// With a [`scheduler_limit`](SimGlobal::scheduler_limit) set (a World does
/// this from its own clock), one call runs every task due up to that tick
/// and never moves firmware time past it; the next wake-up is reported in
/// [`freertos_next_wake`](SimGlobal::freertos_next_wake).  Without a limit,
/// each call makes one scheduling step: resume the task FreeRTOS selected
/// until it yields, or advance virtual time to the next wake-up.
///
/// Returns 1 if the simulation has more work to do, or 0 if nothing can
/// happen any more without external input.
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
/// // Step-by-step loop — equivalent to sim_start_scheduler().
/// while (sim_scheduler_tick()) {
///     // The caller can interleave its own work between steps.
/// }
/// ```
#[no_mangle]
pub unsafe extern "C" fn sim_scheduler_tick() -> u32 {
    // After vTaskEndScheduler() the machine is done: never restart the
    // kernel or advance it (a World may still step it for other machines'
    // events).
    if with_sim_global(|global| global.borrow().freertos_ended) {
        with_sim_global(|global| {
            let mut global = global.borrow_mut();
            global.freertos_quiescent = true;
            global.freertos_next_wake = None;
        });
        flush_trace();
        return 0;
    }
    /*
     * Do not keep this state in a host-thread TLS slot.  A World activates a
     * different Simulator for each machine step, so TLS made the second World
     * inherit the first World's tick time and one-time setup.
     */
    let (mut initialized, mut sim_time) = with_sim_global(|global| {
        let mut global = global.borrow_mut();
        (
            std::mem::take(&mut global.scheduler_initialized),
            std::mem::take(&mut global.scheduler_sim_time),
        )
    });

    if !initialized {
        initialized = true;
        sim_time = 0;
    }
    // Firmware booted by a World usually only creates tasks; start the
    // FreeRTOS scheduler (idle + timer tasks) on its behalf — also when the
    // firmware boots after earlier (native-only) steps.
    freertos::ensure_started(sim_time);

    let start_time = sim_time;
    let (freertos, limit) = with_sim_global(|global| {
        let global = global.borrow();
        (global.freertos, global.scheduler_limit)
    });
    let more = match (freertos, limit) {
        (true, Some(limit)) => {
            let report = freertos::run_until(&mut sim_time, limit);
            with_sim_global(|global| global.borrow_mut().freertos_next_wake = report.next_wake);
            report.more
        }
        _ => run_one_scheduler_cycle(&mut sim_time),
    };
    // Every scheduler path only ever moves virtual time forward.
    debug_assert!(sim_time >= start_time, "virtual time ran backwards");
    with_sim_global(|global| {
        let mut global = global.borrow_mut();
        global.scheduler_initialized = initialized;
        global.scheduler_sim_time = sim_time;
        global.freertos_quiescent = global.freertos && !more;
    });

    // Flush thread-local trace (firmware sim_trace_u32 calls, can_send/recv, etc.)
    // into the active SimGlobal's trace sink so drain_trace_prefixed can see them.
    flush_trace();

    u32::from(more)
}

/// Yield the currently executing task from C code (`portYIELD()`).
///
/// Inside a task with interrupts unmasked, the fiber suspends and the
/// scheduler switches.  Inside a critical section, or from scheduler/ISR
/// context, the switch is pended until interrupts are unmasked or the
/// current scheduling step ends, like PendSV on a Cortex-M.
///
/// # Safety
///
/// Safe to call from any context.  Without an RTOS, calling it outside a
/// fiber records a fatal error but returns gracefully.
#[no_mangle]
pub unsafe extern "C" fn sim_port_yield() {
    if freertos::port_yield() {
        return;
    }
    // Pended: fine inside a critical section or for FreeRTOS called from
    // scheduler/ISR context.  Without an RTOS it is a port bug.
    let freertos = with_sim_global(|g| g.try_borrow().map(|g| g.freertos).unwrap_or(true));
    if !has_active_fiber() && !freertos {
        // Record fatal error via thread-local trace
        TL_TRACE.with(|tl| {
            tl.borrow_mut().push(sim_core::trace::TraceEvent::Fatal {
                at: guest_runtime::active_now(),
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
/// A host I/O wait of the task is cancelled at once, because FreeRTOS frees
/// the TCB of another task as soon as this hook returns.
///
/// # Safety
///
/// Safe to call inside or outside a fiber, but not while `SIM_GLOBAL` is
/// borrowed (the scheduler never holds it while a FreeRTOS task runs).
#[no_mangle]
pub unsafe extern "C" fn sim_task_deleted(task_id: u64) {
    freertos::cancel_io_wait(task_id);
    PENDING_DELETIONS.with(|pd| {
        pd.borrow_mut().push(task_id);
    });
    // Outside a task (host code between steps, a peripheral callback,
    // `vTaskEndScheduler()` deleting the idle and timer tasks) the deletion
    // belongs to the active machine and is applied now.  Left pending, a
    // machine that never steps again (an ended one) would hand it to the
    // next machine stepped on this thread, whose new task may reuse the
    // freed TCB's address.
    if !has_active_fiber() && with_sim_global(|g| g.try_borrow_mut().is_ok()) {
        process_pending_deletions();
    }
}

/// Process pending task deletions recorded by `sim_task_deleted`.
///
/// Must be called from the scheduler main loop (or any context where
/// `SIM_GLOBAL` is safely accessible — i.e., NOT from within a fiber).
/// Drains the thread-local `PENDING_DELETIONS` list and marks each task
/// as `TaskState::Exited` in the global task registry.
///
/// The task's stack is released without unwinding it (see
/// [`sim_fiber::Fiber::release_stack`]): a suspended task's stack is leaked,
/// because values on it may still be borrowed from elsewhere.  It is
/// released after the task table is no longer borrowed: a task that never
/// ran still owns its closure, and the destructors of what it captured may
/// call the simulator.
pub(crate) fn process_pending_deletions() {
    PENDING_DELETIONS.with(|pd| {
        let deleted_ids: Vec<u64> = pd.borrow_mut().drain(..).collect();
        if deleted_ids.is_empty() {
            return;
        }
        // The stacks are only detached under the task-table borrow and
        // released after it: releasing a task that never ran drops its
        // closure, whose captures' destructors may call the simulator
        // (spawn a task, say).
        let released: Vec<sim_fiber::DetachedStack> = with_sim_global(|global| {
            let mut global = global.borrow_mut();
            // Use a set to avoid O(D × T) nested loop when many tasks are deleted.
            let deleted_set: std::collections::BTreeSet<_> = deleted_ids.iter().copied().collect();
            global
                .tasks
                .iter_mut()
                .filter(|task| deleted_set.contains(&task.id))
                .map(|task| task.mark_deleted())
                .collect()
        });
        drop(released);
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
    sleep_current_until(until_ticks);
}

/// Block the running task until tick `until` (yielding once if that has
/// passed), through whichever scheduler owns it.  See [`wait_as_owner`].
pub(crate) fn sleep_current_until(until: Tick) {
    wait_as_owner(
        YieldReason::SleepUntil(until),
        || {},
        || guest_runtime::active_now() >= until,
        // FreeRTOS: block on the kernel's delayed list, or FreeRTOS would
        // keep selecting the task.
        || freertos::delay_current_until(until),
    );
}

/// The one way a task waits: until `satisfied()`, through whichever
/// scheduler owns it *now*.
///
/// Waits at least once (a sleep to a passed deadline still yields), then
/// rechecks `satisfied()` after **every** resume and waits again until it
/// holds.  A resume can come from anywhere: the wait's own wake-up, FreeRTOS
/// adopting a natively waiting task (its new TCB is ready), or the firmware
/// suspending and resuming the TCB (`vTaskSuspend`/`vTaskResume`).
///
/// Each round, `arm()` (re)registers whatever wakes the wait, then:
/// - scheduled by FreeRTOS: `kernel_wait()` blocks the task in the kernel;
/// - otherwise the fiber suspends with `native` until the fiber scheduler
///   resumes it.
///
/// Every fiber-level wait primitive (`sim_task_delay_until`,
/// `TaskContext::sleep_until`/`sleep_for`, `sim_host_block_on_fd`) goes
/// through here.  (Yields and budget preemption carry no wait condition, so
/// resuming them early is correct.)
pub(crate) fn wait_as_owner(
    native: YieldReason,
    arm: impl Fn(),
    satisfied: impl Fn() -> bool,
    kernel_wait: impl Fn(),
) {
    loop {
        arm();
        if freertos::schedules_native_task() {
            kernel_wait();
        } else {
            suspend_active_fiber(native);
        }
        if satisfied() {
            return;
        }
    }
}

/// End the host I/O wait of `task` without readiness: its descriptor was
/// deregistered, so nothing can report it ready any more.  The task
/// returns from `sim_host_block_on_fd()` (see its wait condition) at its
/// next resume: FreeRTOS readies it, a native task becomes runnable.
#[cfg(unix)]
pub(crate) fn end_io_wait(task: TaskId) {
    // Latched for this wait: the task returns at its next resume whatever
    // the descriptor's registration is by then.
    with_sim_global(|global| {
        let mut global = global.borrow_mut();
        if !global.io_cancelled.contains(&task) {
            global.io_cancelled.push(task);
        }
    });
    freertos::resume_io_waiter(task);
    with_sim_global(|global| {
        let mut global = global.borrow_mut();
        for t in global.tasks.iter_mut() {
            if t.id == task && matches!(t.state, sim_fiber::TaskState::IoWaiting) {
                t.set_ready();
            }
        }
        // The last step's report (quiescent, next wake) no longer holds.
        global.note_new_task();
    });
}

/// Consume the cancellation latched for `task` by [`end_io_wait`]: whether
/// its I/O wait was ended without readiness since.
pub(crate) fn take_io_cancelled(task: TaskId) -> bool {
    with_sim_global(|g| {
        let cancelled = &mut g.borrow_mut().io_cancelled;
        let before = cancelled.len();
        cancelled.retain(|&t| t != task);
        cancelled.len() != before
    })
}

/// Consume the readiness latched for `task` by [`host_poll_and_wake`]:
/// whether the descriptor it waits on was reported readable since.
pub(crate) fn take_io_ready(task: TaskId) -> bool {
    with_sim_global(|g| {
        let ready = &mut g.borrow_mut().io_ready;
        let before = ready.len();
        ready.retain(|&t| t != task);
        ready.len() != before
    })
}

/// Enter a virtual critical section.
///
/// The nesting depth belongs to the active machine — safe to call from
/// within a fiber.
///
/// # Safety
///
/// Always safe — only touches the machine's interrupt state.  Can be called
/// from any context.  Callers must pair with `sim_exit_critical`.
#[no_mangle]
pub unsafe extern "C" fn sim_enter_critical() {
    guest_runtime::update_interrupt_state(|s| {
        s.critical_nesting = s.critical_nesting.saturating_add(1);
    });
}

/// Exit a virtual critical section.
///
/// The nesting depth belongs to the active machine — safe to call from
/// within a fiber.
///
/// When the nesting count reaches zero, any deferred virtual interrupts
/// are delivered immediately.
///
/// # Safety
///
/// Can be called from any context.  Must be paired with a prior
/// `sim_enter_critical`.
#[no_mangle]
pub unsafe extern "C" fn sim_exit_critical() {
    let was_locked = is_critical_locked();
    guest_runtime::update_interrupt_state(|s| {
        s.critical_nesting = s.critical_nesting.saturating_sub(1);
    });

    // If we just unlocked (was locked before decrement, now not locked),
    // deliver any pending IRQs that were deferred.
    if was_locked && !is_critical_locked() {
        let now = guest_runtime::active_now();
        freertos::service_masked_ticks();
        deliver_pending_irqs(now);
        freertos::perform_deferred_yield();
    }
}

/// Whether virtual interrupts are currently locked.
pub fn is_critical_locked() -> bool {
    guest_runtime::interrupt_state().masked()
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
    if crate::freertos::fatally_stopped() {
        return;
    }
    let label = if label_ptr.is_null() {
        "?"
    } else {
        let c_str = std::ffi::CStr::from_ptr(label_ptr);
        c_str.to_str().unwrap_or("?")
    };
    let label_static: &'static str = sim_core::trace::intern(label);

    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: guest_runtime::active_now(),
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
    /// task is ready.  On a FreeRTOS machine this is `taskYIELD()`: inside a
    /// critical section the switch is pended until interrupts are unmasked.
    pub fn yield_now(&self) {
        if freertos::schedules_native_task() {
            freertos::port_yield();
        } else {
            suspend_active_fiber(YieldReason::Cooperative);
        }
    }

    /// Sleep until an absolute virtual time.
    ///
    /// The scheduler will not resume this task before `at` ticks.  On a
    /// FreeRTOS machine the task sleeps on the kernel's delayed list, like
    /// `vTaskDelay()`.
    pub fn sleep_until(&self, at: Tick) {
        sleep_current_until(at);
    }

    /// Sleep for a relative number of ticks from now.
    pub fn sleep_for(&self, delta: Tick) {
        let now = guest_runtime::active_now();
        self.sleep_until(now.saturating_add(delta));
    }

    /// Current virtual time in ticks.
    pub fn now(&self) -> Tick {
        guest_runtime::active_now()
    }
}

/// Spawn a Rust task that runs on the same fiber runtime as C tasks.
///
/// The closure `f` executes as the task body inside a stackful coroutine.
/// It receives a [`TaskContext`] for yield/sleep/time operations and can
/// call any re-entrant-safe C ABI function (trace, budget poll, etc.).
///
/// On a machine that runs FreeRTOS (whether the firmware boots before or
/// after the task is spawned) FreeRTOS schedules the task like any of its
/// own: the engine creates a FreeRTOS task for it, at `priority` (clamped
/// to `configMAX_PRIORITIES - 1`), the next time it steps the machine.
/// [`TaskContext::sleep_until`] then blocks on the kernel's delayed list and
/// [`TaskContext::yield_now`] behaves like `taskYIELD()`.
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
                if freertos::schedules_native_task() {
                    // Remove the task from FreeRTOS; never resumed after.
                    freertos::delete_current_task();
                }
                loop {
                    suspend_active_fiber(YieldReason::TaskExit);
                }
            },
        );
        global.tasks.push(fiber);
        global.native_tasks_to_adopt.push((id, None));
        global.note_new_task();
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
        b.entry_count >= b.max_entries && !b.exceeded
    });
    if exceeded {
        charge_budget(line);
    }
}

/// Take the tick interrupt an exhausted budget deferred while an ISR ran
/// on the current task's fiber.
pub(crate) fn poll_deferred_budget() {
    let exceeded = BUDGET.with(|b| {
        let b = b.borrow();
        b.entry_count >= b.max_entries && !b.exceeded
    });
    if exceeded && sim_fiber::has_active_fiber() {
        charge_budget(0);
    }
}

/// The running task used up its budget: yield with `BudgetExceeded` (a
/// tick interrupt).  With interrupts masked or an ISR running the
/// interrupt stays deferred, and the next poll takes it.
fn charge_budget(line: u32) {
    // A tick interrupt cannot switch tasks in the middle of an ISR.  A
    // FreeRTOS task's CPU time is charged even while it masks interrupts:
    // virtual time keeps moving, though the tick interrupt (and any switch)
    // waits for the unmask and the same task resumes.  Without FreeRTOS
    // the engine never preempts, and a masked task keeps the CPU.
    if device_ffi::in_isr() {
        return;
    }
    // The ownership check calls into C (the kernel's current-task
    // accessors), which instrumentation may make re-enter
    // `sim_budget_poll()`: a nested poll during the check charges nothing.
    // No `BUDGET` borrow is held across it.
    thread_local! {
        static CHECKING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if CHECKING.with(|c| c.replace(true)) {
        return;
    }
    let owned = !is_critical_locked() || freertos::owns_current_task();
    CHECKING.with(|c| c.set(false));
    if !owned {
        return;
    }

    // Reset the counter if we're inside a fiber (the yield will succeed
    // and the fiber resumes with a fresh budget).  Outside a fiber (e.g.,
    // unit test), leave the exceeded state for inspection.
    BUDGET.with(|b| {
        let mut b = b.borrow_mut();
        if sim_fiber::has_active_fiber() {
            b.entry_count = 0;
        } else {
            b.exceeded = true;
        }
    });

    let now = guest_runtime::active_now();
    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "budget_exceeded",
            value: line,
        });
    });

    suspend_active_fiber(YieldReason::BudgetExceeded);
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
            // Latch the readiness until the task consumes it: the poller
            // forgets it below, before the task resumes.
            with_sim_global(|global| {
                let mut global = global.borrow_mut();
                if !global.io_ready.contains(task_id) {
                    global.io_ready.push(*task_id);
                }
            });
            // A FreeRTOS task waits in the kernel: FreeRTOS readies it.
            if freertos::resume_io_waiter(*task_id) {
                woken += 1;
            }
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
// ---------------------------------------------------------------------------
// Event queue for virtual peripherals (RTOS-agnostic)
// ---------------------------------------------------------------------------

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
    if crate::freertos::fatally_stopped() {
        return;
    }
    guest_runtime::with_peripheral_events(|q| q.entry(at_cycles).or_default().push(cb));
}

/// Peek the next event deadline from the peripheral event queue.
///
/// Returns `None` if the queue is empty, or `Some(cycle_time)` of the
/// earliest pending event.  The drain loop uses this alongside the RTOS
/// timeout to decide how far to advance virtual time.
pub fn next_event_deadline() -> Option<u64> {
    guest_runtime::with_peripheral_events(|q| q.keys().next().copied())
}

/// The tick a non-FreeRTOS scheduler should dispatch peripheral callbacks
/// at next, never before `sim_time` (a callback scheduled for the past
/// runs now): virtual time never moves backward.
pub fn event_target(sim_time: Tick) -> Option<Tick> {
    next_event_deadline().map(|at| at.max(sim_time))
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
        // A stopped machine (e.g. by an interrupt storm) runs nothing more.
        if freertos::halted() {
            break;
        }
        let next = guest_runtime::with_peripheral_events(|q| {
            let mut entry = q.first_entry()?;
            if *entry.key() > now_cycles {
                return None;
            }
            let cb = entry.get_mut().remove(0);
            if entry.get().is_empty() {
                entry.remove();
            }
            Some(cb)
        });
        let Some(cb) = next else {
            break;
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            cb();
        }));
        // A callback that keeps scheduling callbacks for now (directly, or
        // through an IRQ whose ISR does) is an interrupt storm: past the
        // storm limit the machine stops (`freertos::storm_fatal`).
        if freertos::no_progress_at(now_cycles) {
            break;
        }
    }
}

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
        let data = b"hello";
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
            c.clear(48, 10);
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

    /// Test that an IRQ raised with interrupts unmasked is delivered at once.
    #[test]
    fn test_irq_delivered_when_not_locked() {
        // Clear pending IRQs
        sim_devices::irq::with_irq_mut(|c| {
            c.take_pending();
        });
        TL_TRACE.with(|tl| tl.borrow_mut().clear());

        // A clock of its own: the legacy fallback clock is shared by every
        // test thread.
        let runtime = Rc::new(guest_runtime::GuestRuntime::new());
        let _runtime = guest_runtime::activate_guest_runtime(&runtime);
        set_sim_now(200);

        assert!(!is_critical_locked());

        // Raise an IRQ: unmasked, so it is taken immediately.
        unsafe {
            sim_irq_raise(33);
        }
        assert!(!sim_devices::irq::with_irq(|c| c.is_pending(33)));
        let delivered = TL_TRACE.with(|tl| {
            tl.borrow().iter().any(|e| {
                matches!(
                    e,
                    sim_core::trace::TraceEvent::InterruptDelivered { at: 200, irq: 33 }
                )
            })
        });
        assert!(delivered);

        // Nothing left to deliver.
        let delivered = unsafe { sim_irq_deliver_pending(200) };
        assert_eq!(delivered, 0);
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

        // A clock of its own: the legacy fallback clock is shared by every
        // test thread.
        let runtime = Rc::new(guest_runtime::GuestRuntime::new());
        let _runtime = guest_runtime::activate_guest_runtime(&runtime);
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

        // Step 3: After sleep, wake at tick 3 and resume → task exits.
        set_sim_now(3);
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

    /// Regression for the native port's C singleton state.  `c_sim_main`
    /// creates FreeRTOS tasks and queues, runs them, then returns.  Repeating
    /// that lifecycle with two independently activated Simulators exercises
    /// both interleaving and normal Drop/recreation without process exit.
    #[test]
    fn two_contexts_preserve_independent_ticks() {
        unsafe extern "C" {
            fn sim_advance_ticks(count: u32) -> u32;
            fn xTaskGetTickCount() -> u32;
        }
        let mut a = simulator::Simulator::new(sim_core::SimConfig::default());
        let mut b = simulator::Simulator::new(sim_core::SimConfig::default());
        {
            let _g = a.activate();
            unsafe {
                sim_advance_ticks(10);
            }
            assert_eq!(unsafe { xTaskGetTickCount() }, 10);
        }
        {
            let _g = b.activate();
            unsafe {
                sim_advance_ticks(3);
            }
            assert_eq!(unsafe { xTaskGetTickCount() }, 3);
        }
        {
            let _g = a.activate();
            assert_eq!(
                unsafe { xTaskGetTickCount() },
                10,
                "A tick must be restored after B ran"
            );
            unsafe {
                sim_advance_ticks(5);
            }
            assert_eq!(unsafe { xTaskGetTickCount() }, 15);
        }
        {
            let _g = b.activate();
            assert_eq!(unsafe { xTaskGetTickCount() }, 3, "B tick must be restored");
        }
    }

    #[test]
    fn tick_advances_under_freertos_context() {
        unsafe extern "C" {
            fn sim_advance_ticks(count: u32) -> u32;
            fn xTaskGetTickCount() -> u32;
        }
        let mut sim = simulator::Simulator::new(sim_core::SimConfig::default());
        let _active = sim.activate();
        let before = unsafe { xTaskGetTickCount() };
        unsafe {
            sim_advance_ticks(123);
        }
        let after = unsafe { xTaskGetTickCount() };
        assert_eq!(before, 0, "fresh context tick should start at 0");
        assert_eq!(
            after, 123,
            "sim_advance_ticks must advance xTickCount, got {after}"
        );
        drop(_active);
        let _active2 = sim.activate();
        let restored = unsafe { xTaskGetTickCount() };
        assert_eq!(
            restored, 123,
            "tick must survive deactivate/activate, got {restored}"
        );
    }

    #[test]
    fn two_freertos_worlds_drop_and_recreate_100x() {
        unsafe extern "C" {
            fn c_sim_main() -> std::ffi::c_int;
        }

        for _ in 0..100 {
            let mut first = simulator::Simulator::new(sim_core::SimConfig::default());
            let mut second = simulator::Simulator::new(sim_core::SimConfig::default());

            // `c_sim_main` drains its scheduler synchronously.  Alternating
            // activation is still essential: it verifies each execution uses
            // its own saved C kernel lists/TCB bridge rather than the previous
            // Simulator's singleton state.
            {
                let _active = first.activate();
                assert_eq!(unsafe { c_sim_main() }, 0);
            }
            {
                let _active = second.activate();
                assert_eq!(unsafe { c_sim_main() }, 0);
            }
        }
    }
}
