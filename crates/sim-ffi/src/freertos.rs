//! FreeRTOS-driven scheduling: the kernel decides, the engine switches fibers.
//!
//! Every FreeRTOS task owns one fiber, created by the `traceTASK_CREATE` hook
//! (at start-up or at runtime).  The engine never picks a task itself: after
//! a task yields it runs `vTaskSwitchContext()` — what PendSV does on a
//! Cortex-M — and resumes the fiber of whatever task FreeRTOS put in
//! `pxCurrentTCB`.  Blocking, priorities, time slicing, mutex priority
//! inheritance and software timers are therefore exactly FreeRTOS's.
//!
//! Virtual time only moves when the idle task runs (every application task
//! is blocked) or when a task exhausts its instrumentation budget, which is
//! treated as one tick of CPU time.

use sim_core::time::Tick;
use sim_core::trace::TraceEvent;
use sim_fiber::yield_reason::YieldReason;
use sim_fiber::{has_active_fiber, suspend_active_fiber, Fiber, TaskId};

use crate::device_ffi::deliver_pending_irqs;
use crate::net_ffi::eth_loopback_bridge;
use crate::{
    dispatch_events, guest_runtime, host_poll_and_wake, next_event_deadline,
    process_pending_deletions, resume_task, set_sim_now, with_sim_global, TL_TRACE,
};

#[link(name = "embedded_c_payload", kind = "static")]
extern "C" {
    fn vTaskSwitchContext();
    fn sim_freertos_current_handle() -> u64;
    fn sim_freertos_current_is_idle() -> u32;
    fn sim_freertos_current_is_deleted() -> u32;
    fn sim_freertos_ticks_until_unblock() -> u64;
    fn sim_freertos_scheduler_running() -> u32;
    fn sim_freertos_timers_in_use() -> u32;
    fn sim_freertos_start_external();
    fn sim_freertos_set_tick_count(ticks: u32);
    fn sim_freertos_retire_current();
    fn sim_port_task_returned();
    fn sim_advance_ticks(count: u32) -> u32;
    fn sim_freertos_tick_rate_hz() -> u32;
    fn sim_freertos_adopt_native(name: *const std::ffi::c_char, priority: u32) -> u32;
    fn vTaskDelay(ticks: u32);
    fn vTaskDelete(task: *mut std::ffi::c_void);
}

thread_local! {
    /// Set while [`adopt_native_tasks`] creates the FreeRTOS task for an
    /// existing native fiber: `traceTASK_CREATE` binds the new TCB to that
    /// fiber instead of creating one.
    static ADOPTING: std::cell::Cell<Option<TaskId>> = const { std::cell::Cell::new(None) };
}

// Host I/O waits: the host poller is Unix-only.
#[cfg(unix)]
#[link(name = "embedded_c_payload", kind = "static")]
extern "C" {
    fn vTaskSuspend(task: *mut std::ffi::c_void);
    fn vTaskResume(task: *mut std::ffi::c_void);
    fn xTaskGetCurrentTaskHandle() -> *mut std::ffi::c_void;
}

/// Perform a yield that was deferred while interrupts were masked.
///
/// Called when interrupts become unmasked.  Inside a task this suspends the
/// fiber; in scheduler context the switch is left for the engine, which
/// always runs `vTaskSwitchContext()` after a task slice.
pub(crate) fn perform_deferred_yield() {
    if has_active_fiber()
        && guest_runtime::update_interrupt_state(|s| std::mem::take(&mut s.yield_pending))
    {
        suspend_active_fiber(YieldReason::RtosPortYield);
    }
}

/// `portYIELD()`: switch now, or pend the switch until interrupts are
/// unmasked (inside a critical section) or until the current engine step
/// ends (called from scheduler/ISR context).
///
/// Returns `false` if the yield was pended.
pub(crate) fn port_yield() -> bool {
    if crate::is_critical_locked() || !has_active_fiber() {
        guest_runtime::update_interrupt_state(|s| s.yield_pending = true);
        return false;
    }
    suspend_active_fiber(YieldReason::RtosPortYield)
}

/// Whether the running fiber is the task FreeRTOS selected, i.e. FreeRTOS
/// (not the fiber scheduler) decides when it runs again.
pub(crate) fn owns_current_task() -> bool {
    if !has_active_fiber() {
        return false;
    }
    // Safety: plain reads of the active machine's kernel state.
    unsafe {
        sim_freertos_scheduler_running() != 0
            && sim_freertos_current_handle() == guest_runtime::active_task_id()
    }
}

/// Whether FreeRTOS schedules the running native Rust task
/// ([`crate::spawn_rust_task`]).
pub(crate) fn schedules_native_task() -> bool {
    // Outside a Simulator step (unit tests resuming a fiber by hand) the
    // task table is borrowed and no FreeRTOS kernel runs this fiber.
    with_sim_global(|g| g.try_borrow().map(|g| g.freertos).unwrap_or(false)) && owns_current_task()
}

/// Delete the running FreeRTOS task; the fiber is never resumed again.
pub(crate) fn delete_current_task() {
    // Safety: called from the running FreeRTOS task.
    unsafe { vTaskDelete(std::ptr::null_mut()) };
}

/// Give every native Rust task spawned on this machine a FreeRTOS task of
/// its own, so FreeRTOS schedules it alongside the firmware's tasks.
/// Does nothing until the machine runs FreeRTOS.  Returns `true` if a task
/// was adopted (FreeRTOS may then want to switch to it).
///
/// Must be called from scheduler context with the machine's kernel active.
pub(crate) fn adopt_native_tasks() -> bool {
    let pending: Vec<(TaskId, &'static str, u32)> = with_sim_global(|g| {
        let mut g = g.borrow_mut();
        if !g.freertos || g.native_tasks_to_adopt.is_empty() {
            return Vec::new();
        }
        let ids = std::mem::take(&mut g.native_tasks_to_adopt);
        ids.into_iter()
            .filter_map(|(id, _)| {
                g.tasks
                    .iter()
                    .find(|t| t.id == id && !t.is_terminated())
                    .map(|t| (id, t.name, t.priority))
            })
            .collect()
    });
    for &(id, name, priority) in &pending {
        let c_name = std::ffi::CString::new(name.replace('\0', "")).unwrap_or_default();
        ADOPTING.with(|a| a.set(Some(id)));
        // Safety: scheduler context, machine kernel active; `c_name` lives
        // across the call (FreeRTOS copies it).
        let created = unsafe { sim_freertos_adopt_native(c_name.as_ptr(), priority) } != 0;
        let unbound = ADOPTING.with(|a| a.take()).is_some();
        assert!(
            created && !unbound,
            "costar: FreeRTOS could not create a task for native Rust task `{name}` \
             (kernel heap exhausted?)"
        );
    }
    !pending.is_empty()
}

/// `sim_task_delay_until()` from a FreeRTOS task: block it on the kernel's
/// delayed list until tick `until` (an immediate yield if that has passed).
pub(crate) fn delay_current_until(until: Tick) {
    loop {
        let remaining = until.saturating_sub(guest_runtime::active_now());
        let ticks = remaining.min(u64::from(u32::MAX - 1)) as u32;
        // Safety: called from the running FreeRTOS task.
        unsafe { vTaskDelay(ticks) };
        if u64::from(ticks) == remaining {
            return;
        }
    }
}

/// `sim_host_block_on_fd()` from a FreeRTOS task: suspend it in the kernel
/// until the host poller reports its descriptor ready, so FreeRTOS runs the
/// machine's other tasks meanwhile.
#[cfg(unix)]
pub(crate) fn block_current_on_io(task: TaskId) {
    // Safety: called from the running FreeRTOS task.
    let tcb = unsafe { xTaskGetCurrentTaskHandle() };
    with_sim_global(|g| g.borrow_mut().freertos_io_waits.push((task, tcb as usize)));
    // Safety: as above; returns once the task is resumed.
    unsafe { vTaskSuspend(std::ptr::null_mut()) };
    // Normally already removed by `resume_io_waiter`; not if the firmware
    // resumed the task itself.
    with_sim_global(|g| {
        g.borrow_mut()
            .freertos_io_waits
            .retain(|&(id, _)| id != task)
    });
}

/// Forget the I/O wait of `task` as FreeRTOS deletes it (called from
/// `traceTASK_DELETE`, before the TCB is freed): no later descriptor
/// readiness may resume the freed TCB or keep the machine alive.
pub(crate) fn cancel_io_wait(task: TaskId) {
    let waited = with_sim_global(|g| {
        let mut g = g.borrow_mut();
        g.io_ready.retain(|&id| id != task);
        let waits = &mut g.freertos_io_waits;
        let before = waits.len();
        waits.retain(|&(id, _)| id != task);
        waits.len() != before
    });
    #[cfg(unix)]
    if waited {
        let _ = sim_net::host_poller::with_existing_host_poller_mut(|hp| hp.forget_task(task));
    }
    #[cfg(not(unix))]
    let _ = waited;
}

/// Ready a FreeRTOS task waiting in [`block_current_on_io`].  Returns
/// `false` if `task` is not such a task.
#[cfg(unix)]
pub(crate) fn resume_io_waiter(task: TaskId) -> bool {
    let tcb = with_sim_global(|g| {
        let mut g = g.borrow_mut();
        let pos = g.freertos_io_waits.iter().position(|&(id, _)| id == task)?;
        Some(g.freertos_io_waits.remove(pos).1)
    });
    let Some(tcb) = tcb else {
        return false;
    };
    // Deletion cancels the wait (`cancel_io_wait`); never hand FreeRTOS a
    // handle whose task has gone.
    if !task_is_live(task) {
        return false;
    }
    // Safety: the TCB belongs to the active machine's kernel and is
    // suspended; called from scheduler context.
    unsafe { vTaskResume(tcb as *mut std::ffi::c_void) };
    true
}

/// Whether `task` exists and has not been deleted.
#[cfg(unix)]
fn task_is_live(task: TaskId) -> bool {
    let deleting = crate::PENDING_DELETIONS.with(|pd| pd.borrow().contains(&task));
    !deleting
        && with_sim_global(|g| {
            g.borrow()
                .tasks
                .iter()
                .any(|t| t.id == task && !matches!(t.state, sim_fiber::TaskState::Exited))
        })
}

/// Whether the firmware called `vTaskEndScheduler()`.
fn ended() -> bool {
    with_sim_global(|g| g.borrow().freertos_ended)
}

/// Run `vTaskSwitchContext()` unless interrupts are masked.
///
/// The one gate for every switch the engine makes on a live task: after a
/// task's slice, a tick interrupt (charged for a used-up budget, or owed
/// from the previous step), input delivered between steps, a host I/O
/// wake-up, and the parked idle task of a bounded step.  While interrupts
/// are masked (a critical section, `portDISABLE_INTERRUPTS()`, also when
/// host code masked them between steps) a switch is held off as PendSV
/// would be: the request is latched in `yield_pending` and the selected
/// task keeps running.  Unmasking performs it (`perform_deferred_yield` in
/// a task, `yield_requested` at the next step).
fn switch_context_after_isrs() {
    if crate::is_critical_locked() {
        guest_runtime::update_interrupt_state(|s| s.yield_pending = true);
    } else {
        switch_context();
    }
}

/// The task at `idx` suspended itself (`portYIELD()`, blocking).  If
/// interrupts are masked the switch is latched like any other (see
/// [`switch_context_after_isrs`]) and the task continues.  A task that can
/// no longer run (deleted, finished) is switched away from at once.
fn switch_after_task_yield(idx: usize) {
    let alive = with_sim_global(|g| {
        g.borrow()
            .tasks
            .get(idx)
            .is_some_and(|t| !t.is_terminated())
    });
    if alive {
        switch_context_after_isrs();
    } else {
        switch_context();
    }
}

/// Run `vTaskSwitchContext()`: FreeRTOS selects the next task.  Only for a
/// task that can no longer run; every other switch goes through
/// [`switch_context_after_isrs`].
fn switch_context() {
    guest_runtime::update_interrupt_state(|s| s.yield_pending = false);
    // Safety: called from scheduler context with the machine's kernel active.
    unsafe { vTaskSwitchContext() };
}

// ---------------------------------------------------------------------------
// C ABI
// ---------------------------------------------------------------------------

/// Create the fiber for a FreeRTOS task (called by `traceTASK_CREATE`).
///
/// # Safety
///
/// `name` must be null or a valid C string; `entry(arg)` must be the task
/// function FreeRTOS was given.  May be called from a running task.
#[no_mangle]
pub unsafe extern "C" fn sim_freertos_task_created(
    _tcb: *mut std::ffi::c_void,
    name: *const std::ffi::c_char,
    entry: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    arg: *mut std::ffi::c_void,
    requested_stack_words: u32,
    priority: u32,
) -> usize {
    // A native Rust task being adopted keeps its own fiber.
    if let Some(id) = ADOPTING.with(|a| a.take()) {
        return id as usize;
    }
    // Legacy pattern in reverse order: `sim_create_task(entry, arg)`
    // already created the fiber for this task; bind the TCB to it instead
    // of creating a second one that would run the task twice.  Only a live
    // fiber qualifies: one that already ran to completion is another task.
    if let Some(entry) = entry {
        let key = Some((entry as usize, arg as usize));
        let bound = with_sim_global(|global| {
            let mut global = global.borrow_mut();
            let crate::SimGlobal {
                native_tasks_to_adopt: pending,
                tasks,
                ..
            } = &mut *global;
            let pos = pending.iter().position(|&(id, e)| {
                e == key && tasks.iter().any(|t| t.id == id && !t.is_terminated())
            })?;
            let id = pending.remove(pos).0;
            global.freertos = true;
            global.note_new_task();
            Some(id)
        });
        if let Some(id) = bound {
            return id as usize;
        }
    }

    let name = if name.is_null() {
        "unnamed"
    } else {
        std::ffi::CStr::from_ptr(name).to_str().unwrap_or("unnamed")
    };
    let name = sim_core::trace::intern(name);
    let entry = entry.expect("sim_freertos_task_created: NULL task function");

    with_sim_global(|global| {
        let mut global = global.borrow_mut();
        global.freertos = true;

        let id: TaskId = global.next_task_id;
        global.next_task_id += 1;

        let fiber = Fiber::new(
            id,
            name,
            priority,
            requested_stack_words,
            sim_fiber::MIN_HOST_COROUTINE_STACK,
            id,
            move |_reason| {
                // Safety: running inside the task's fiber.
                unsafe {
                    entry(arg);
                    // FreeRTOS tasks must not return; delete the task.
                    sim_port_task_returned();
                }
                // vTaskDelete(NULL) switched away for good; never resumed.
                loop {
                    suspend_active_fiber(YieldReason::TaskExit);
                }
            },
        );
        // The stack is leaked, not freed, if the task is deleted while
        // suspended: its C code may have called Rust code that holds borrows
        // of it (see `Fiber::assume_reclaimable_stack`).
        global.tasks.push(fiber);
        global.note_new_task();
        global.unclaimed_freertos_tasks.push(crate::UnclaimedTask {
            id,
            entry: entry as usize,
            arg: arg as usize,
            name,
        });

        if let Some(ref mut trace) = global.trace {
            trace.record(TraceEvent::TaskCreated {
                at: guest_runtime::active_now(),
                task: id,
                name,
            });
        }
        id as usize
    })
}

/// A task was made ready (`traceMOVED_TASK_TO_READY_STATE`, i.e. any path
/// through `prvAddTaskToReadyList()`).
///
/// Between scheduling steps (e.g. `vTaskResume()`, `xTaskNotifyGive()` or
/// `xSemaphoreGive()` from `Firmware::step` after it ran the scheduler)
/// this invalidates the last step's result — quiescent, next wake-up — so
/// the machine runs again.  Inside a step the step's own report replaces it.
#[no_mangle]
pub extern "C" fn sim_freertos_task_readied() {
    with_sim_global(|g| {
        // Called from deep inside the kernel; if the engine holds the task
        // table (it never does while calling into the kernel), skip: the
        // current step's report supersedes this anyway.
        if let Ok(mut g) = g.try_borrow_mut() {
            g.note_new_task();
        }
    });
}

/// Returns non-zero when `xPortStartScheduler()` must return immediately
/// because a Simulator (e.g. a World machine) steps the scheduler.
#[no_mangle]
pub extern "C" fn sim_port_start_scheduler() -> u32 {
    u32::from(crate::has_active_simulator())
}

/// `vPortEndScheduler()`: stop scheduling this machine.
#[no_mangle]
pub extern "C" fn sim_port_end_scheduler() {
    // vTaskEndScheduler() masked interrupts; the machine is done, leave
    // its interrupt state clean.
    guest_runtime::update_interrupt_state(|s| *s = Default::default());
    with_sim_global(|g| {
        // From host code between steps the task table is free; from a
        // task too (the engine never holds it while a task runs).
        let mut g = g.borrow_mut();
        g.freertos_ended = true;
        // Nothing can wake an ended machine again.
        g.freertos_next_wake = None;
        g.freertos_quiescent = true;
    });
    if has_active_fiber() {
        // The calling task never runs again.
        loop {
            suspend_active_fiber(YieldReason::TaskExit);
        }
    }
}

/// Idle-task hook: hand control back to the engine so it can advance time.
#[no_mangle]
pub extern "C" fn sim_port_idle() {
    suspend_active_fiber(YieldReason::Idle);
}

/// `portYIELD_FROM_ISR(pdTRUE)`.
#[no_mangle]
pub extern "C" fn sim_port_yield_from_isr() {
    // From task context this is an ordinary yield; from ISR/scheduler
    // context the engine switches once the ISR returns.
    port_yield();
}

/// `portDISABLE_INTERRUPTS()`.
#[no_mangle]
pub extern "C" fn sim_disable_interrupts() {
    guest_runtime::update_interrupt_state(|s| s.disabled = true);
}

/// `portENABLE_INTERRUPTS()`.
#[no_mangle]
pub extern "C" fn sim_enable_interrupts() {
    guest_runtime::update_interrupt_state(|s| s.disabled = false);
    if !crate::is_critical_locked() {
        service_masked_ticks();
        deliver_pending_irqs(guest_runtime::active_now());
        perform_deferred_yield();
    }
}

/// `configASSERT()` failed.
///
/// # Safety
///
/// `file` must be null or a valid C string.
#[no_mangle]
pub unsafe extern "C" fn sim_assert_failed(file: *const std::ffi::c_char, line: u32) {
    let file = if file.is_null() {
        "?"
    } else {
        std::ffi::CStr::from_ptr(file).to_str().unwrap_or("?")
    };
    eprintln!("costar: FreeRTOS assertion failed at {file}:{line}");
    let at = guest_runtime::active_now();
    TL_TRACE.with(|tl| {
        let mut tl = tl.borrow_mut();
        tl.push(TraceEvent::Fatal {
            at,
            code: sim_core::error::SimErrorCode::PortFatal,
        });
        tl.push(TraceEvent::UserU32 {
            at,
            label: "assert_failed_line",
            value: line,
        });
    });
    // Stop the offending task; the engine removes it from scheduling.
    if has_active_fiber() {
        loop {
            suspend_active_fiber(YieldReason::Fault);
        }
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Start FreeRTOS for a step-driven Simulator if the firmware did not call
/// `vTaskStartScheduler()` itself.  Creates the idle and timer tasks.
///
/// Checked on every scheduler step, not only the first: firmware may boot
/// after the machine already ran native tasks.  The kernel's tick count
/// then starts at the current virtual time `sim_time`.
pub(crate) fn ensure_started(sim_time: Tick) {
    let has_tasks = with_sim_global(|g| g.borrow().freertos);
    // Safety: scheduler context, machine kernel active.
    unsafe {
        let used = has_tasks || sim_freertos_timers_in_use() != 0;
        if used && sim_freertos_scheduler_running() == 0 {
            sim_freertos_start_external();
            if sim_time != 0 {
                sim_freertos_set_tick_count(sim_time as u32);
            }
        }
    }
}

/// Tasks slices a World-driven machine may run at one tick before the
/// engine charges a tick of CPU time (as if a tick interrupt fired).  Keeps
/// tasks that yield to each other forever from freezing virtual time.
const SLICES_PER_TICK: u32 = 10_000;

/// Result of [`run_until`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunReport {
    /// `false` if nothing can happen any more without external input.
    pub more: bool,
    /// Tick at which the scheduler needs to run next.
    pub next_wake: Option<Tick>,
}

impl RunReport {
    /// Nothing more to do: the scheduler ended, or only external input can
    /// wake the machine.
    pub(crate) const DONE: RunReport = RunReport {
        more: false,
        next_wake: None,
    };
}

/// The task FreeRTOS selected, as `(index in the task table, fiber state)`.
///
/// A task whose fiber can never run again (it faulted, or finished without
/// FreeRTOS deleting it) stays suspended in the kernel; if the firmware
/// resumes it anyway (`vTaskResume()` on its handle), FreeRTOS may select
/// it.  It is then suspended again and FreeRTOS selects another task: a
/// dead task never ends the machine while live ones remain.  `None` only if
/// FreeRTOS selected no task at all.
fn current_task() -> Option<(usize, sim_fiber::TaskState)> {
    let mut retired: Option<u64> = None;
    loop {
        // Safety: scheduler context, machine kernel active.
        let handle = unsafe { sim_freertos_current_handle() };
        if handle == 0 {
            return None;
        }
        if retired == Some(handle) {
            // The kernel would not move off a dead task (it could not be
            // suspended): never retry forever — stop this machine, once,
            // with a fatal report.
            end_machine_fatally();
            return None;
        }
        let found = with_sim_global(|global| {
            let global = global.borrow();
            global.tasks.iter().position(|t| t.id == handle).map(|idx| {
                (
                    idx,
                    global.tasks[idx].state,
                    global.tasks[idx].is_terminated(),
                )
            })
        });
        match found {
            Some((idx, state, false)) => return Some((idx, state)),
            // Safety: as above; the selected task is the dead one.
            Some((_, _, true)) if unsafe { sim_freertos_current_is_deleted() } != 0 => {
                // Deleted (by host code or a callback, while it was the
                // selected task): its TCB waits on the termination list
                // for the idle task to free it.  Never touch it; let
                // FreeRTOS select another task.  The switch cannot wait —
                // the task is gone — but if interrupts are masked the task
                // selected now must not run before the unmask: the machine
                // idles masked meanwhile, as a latched switch would.
                switch_context();
                if crate::is_critical_locked() {
                    with_sim_global(|g| g.borrow_mut().freertos_held_by_mask = true);
                }
                retired = Some(handle);
            }
            Some((_, _, true)) => {
                // A fiber that can never run again (it faulted, or ended
                // without FreeRTOS deleting it) whose TCB is live: suspend
                // it in the kernel.
                // Safety: as above; the selected task is the dead one.
                unsafe { sim_freertos_retire_current() };
                switch_context();
                retired = Some(handle);
            }
            None => return None,
        }
    }
}

/// Stop this machine for good after an unrecoverable kernel state, with
/// one `PortFatal` trace event.  Later steps report completion.
fn end_machine_fatally() {
    let at = guest_runtime::active_now();
    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(TraceEvent::Fatal {
            at,
            code: sim_core::error::SimErrorCode::PortFatal,
        })
    });
    with_sim_global(|g| {
        let mut g = g.borrow_mut();
        g.freertos_ended = true;
        g.freertos_next_wake = None;
        g.freertos_quiescent = true;
    });
}

/// Whether the selected task may not run yet: FreeRTOS had to switch away
/// from a deleted task while interrupts were masked, and the task it
/// selected waits for the unmask (the machine idles masked meanwhile).
/// Cleared once interrupts are unmasked.
fn held_by_mask() -> bool {
    with_sim_global(|g| {
        let mut g = g.borrow_mut();
        if g.freertos_held_by_mask && !crate::is_critical_locked() {
            g.freertos_held_by_mask = false;
        }
        g.freertos_held_by_mask
    })
}

/// Whether FreeRTOS selected its idle task.
fn idle_is_current() -> bool {
    // Safety: scheduler context, machine kernel active.
    unsafe { sim_freertos_current_is_idle() != 0 }
}

/// Whether a task is blocked in a host I/O call.
fn io_waiting() -> bool {
    with_sim_global(|global| !global.borrow().freertos_io_waits.is_empty())
}

/// Poll host descriptors (waiting no later than wall-clock `deadline`
/// ticks) and ready the tasks whose I/O is ready.  Returns `true` if a task
/// became ready; FreeRTOS has then selected the next task.
fn poll_host_io(sim_time: Tick, deadline: Option<Tick>) -> bool {
    let woken = host_poll_and_wake(sim_time, deadline) > 0;
    deliver_pending_irqs(sim_time);
    if ended() {
        return false;
    }
    if woken {
        switch_context_after_isrs();
    }
    woken
}

/// Run the current task for one slice.  Returns `None` if the firmware
/// ended the scheduler during the slice.
fn run_slice(idx: usize, sim_time: Tick) -> Option<Option<YieldReason>> {
    let reason = resume_task(idx, sim_time);
    process_pending_deletions();
    if ended() {
        return None;
    }
    deliver_pending_irqs(sim_time);
    eth_loopback_bridge();
    if matches!(
        reason,
        Some(YieldReason::Fault) | Some(YieldReason::TaskExit) | None
    ) {
        // The task faulted (or ended without deleting itself): stop
        // scheduling it, keep the rest of the system running.  It may
        // have stopped with interrupts masked; they stay usable.
        guest_runtime::update_interrupt_state(|s| *s = Default::default());
        // Safety: scheduler context, machine kernel active.
        unsafe { sim_freertos_retire_current() };
    }
    Some(reason)
}

/// Next tick at which something is due: a delayed task (or tick-counter
/// wrap) or a peripheral event.
fn next_due(sim_time: Tick) -> Option<Tick> {
    // Safety: scheduler context, machine kernel active.
    let until_unblock = unsafe { sim_freertos_ticks_until_unblock() };
    let wake = (until_unblock != u64::MAX).then(|| sim_time + until_unblock.max(1));
    match (wake, next_event_deadline()) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Advance to `target`, fire what is due there and let FreeRTOS reschedule.
fn advance_and_dispatch(sim_time: &mut Tick, target: Tick) {
    if target > *sim_time {
        advance_ticks(sim_time, target - *sim_time);
    }
    set_sim_now(*sim_time);
    dispatch_events(*sim_time);
    // A callback may have ended the scheduler: no kernel call after that.
    if ended() {
        return;
    }
    deliver_pending_irqs(*sim_time);
    // So may an ISR.
    if ended() {
        return;
    }
    switch_context_after_isrs();
}

/// Run one FreeRTOS scheduling step: resume the task FreeRTOS selected until
/// it yields, or — when only the idle task can run — advance virtual time to
/// the next wake-up.
///
/// Used when nothing bounds virtual time (standalone firmware, or a caller
/// that did not set [`SimGlobal::scheduler_limit`](crate::SimGlobal)).
/// Returns `false` when nothing can happen any more without external input.
pub(crate) fn cycle(sim_time: &mut Tick) -> bool {
    process_pending_deletions();
    if ended() {
        return false;
    }
    // "Parked" describes where a bounded step left the idle task; once an
    // unbounded step runs, it no longer holds, and must not make the next
    // bounded step switch tasks without a kernel switch request.
    with_sim_global(|g| g.borrow_mut().freertos_parked = false);
    catch_up_masked_ticks();
    // A budget exhausted at an earlier bounded (World) step's limit owes a
    // tick interrupt; take it before anything runs.
    if with_sim_global(|g| std::mem::take(&mut g.borrow_mut().freertos_tick_owed)) {
        budget_tick(sim_time);
        // A callback due at the tick may have ended the scheduler.
        if ended() {
            return false;
        }
    }
    // Adopting a native task readies it like `xTaskCreate()`: FreeRTOS
    // requests a switch only if it outranks the running task.
    adopt_native_tasks();
    if yield_requested() {
        switch_context_after_isrs();
    }
    let Some((idx, _)) = current_task() else {
        return false;
    };

    // A task held off by the mask (see `held_by_mask`) does not run: the
    // machine idles masked, as if every task were blocked.
    let reason = if held_by_mask() {
        Some(YieldReason::Idle)
    } else {
        let Some(reason) = run_slice(idx, *sim_time) else {
            return false;
        };
        reason
    };

    match reason {
        Some(YieldReason::Idle) => {
            switch_context_after_isrs();
            if idle_is_current() || held_by_mask() {
                if crate::is_critical_locked() {
                    // Masked (by host code between steps) with every task
                    // blocked or held off: the tick interrupt cannot wake
                    // a task and no switch can happen until the unmask, but
                    // peripheral callbacks still run at their deadlines.
                    return match next_event_deadline() {
                        Some(at) => {
                            advance_and_dispatch(sim_time, at.max(*sim_time));
                            !ended()
                        }
                        None => false,
                    };
                }
                // Every application task is blocked.
                return wait_for_next_event(sim_time);
            }
        }
        Some(YieldReason::BudgetExceeded) => {
            budget_tick(sim_time);
            if ended() {
                return false;
            }
        }
        _ => switch_after_task_yield(idx),
    }
    set_sim_now(*sim_time);
    true
}

/// The running task burnt a tick's worth of CPU: deliver a tick interrupt
/// so time moves and higher-priority tasks can preempt.
fn budget_tick(sim_time: &mut Tick) {
    // Callbacks due at the current tick run at it, before the tick.
    dispatch_events(*sim_time);
    // A callback may have ended the scheduler: no kernel call after that.
    if ended() {
        return;
    }
    let tick_switch = advance_ticks(sim_time, 1);
    set_sim_now(*sim_time);
    dispatch_events(*sim_time);
    if ended() {
        return;
    }
    deliver_pending_irqs(*sim_time);
    if ended() {
        return;
    }
    switch_if_requested(tick_switch);
}

/// Run the machine until every task is blocked past tick `limit`, or until
/// running further would move virtual time beyond `limit`.
///
/// Used by Worlds, which derive `limit` from their own clock: firmware time
/// never runs ahead of the World, and all work due at the current instant
/// happens in one call.
pub(crate) fn run_until(sim_time: &mut Tick, limit: Tick) -> RunReport {
    const DONE: RunReport = RunReport::DONE;
    let mut slices = 0u32;

    catch_up_masked_ticks();
    // A budget exhausted at the previous step's limit owes a tick
    // interrupt: take it before anything runs, so a task due at the next
    // tick preempts the busy one exactly as in standalone stepping.
    let owed = with_sim_global(|g| std::mem::take(&mut g.borrow_mut().freertos_tick_owed));
    if owed {
        if *sim_time < limit {
            if let Some(report) = charge_tick(sim_time, limit) {
                return report;
            }
        } else {
            // Still at the same tick (another World event within it): the
            // budget used up the rest of this tick, so no task runs before
            // the tick is charged.  Callbacks due now still run on time.
            dispatch_events(*sim_time);
            // A callback may have ended the scheduler: done, no wake.
            if ended() {
                return DONE;
            }
            deliver_pending_irqs(*sim_time);
            if ended() {
                return DONE;
            }
            with_sim_global(|g| g.borrow_mut().freertos_tick_owed = true);
            return RunReport {
                more: true,
                next_wake: Some(*sim_time + 1),
            };
        }
    }

    loop {
        process_pending_deletions();
        if ended() {
            return DONE;
        }

        catch_up_masked_ticks();
        adopt_native_tasks();
        let parked = with_sim_global(|g| std::mem::take(&mut g.borrow_mut().freertos_parked));
        if parked || yield_requested() {
            // Input delivered since the last call (a World event, an ISR)
            // may have readied a task.
            switch_context_after_isrs();
        }
        let Some((idx, _)) = current_task() else {
            return DONE;
        };

        // A task held off by the mask (see `held_by_mask`) does not run:
        // the machine idles masked, as if every task were blocked.
        let reason = if (parked && idle_is_current()) || held_by_mask() {
            Some(YieldReason::Idle)
        } else {
            match run_slice(idx, *sim_time) {
                Some(reason) => reason,
                None => return DONE,
            }
        };
        slices += 1;

        match reason {
            Some(YieldReason::Idle) => {
                switch_context_after_isrs();
                if !idle_is_current() && !held_by_mask() {
                    continue;
                }
                if crate::is_critical_locked() {
                    // Masked (by host code between steps) with every task
                    // blocked or held off: the tick interrupt cannot wake
                    // a task and no switch can happen until the unmask, but
                    // peripheral callbacks still run at their deadlines.
                    // Firmware time keeps in step with the World; the
                    // ticks are serviced at the unmask, which wakes the
                    // machine again.
                    let next_event = next_event_deadline().map(|at| at.max(*sim_time));
                    match next_event {
                        Some(at) if at <= limit => {
                            advance_and_dispatch(sim_time, at);
                            slices = 0;
                            continue;
                        }
                        _ => {
                            if limit > *sim_time {
                                advance_ticks(sim_time, limit - *sim_time);
                            }
                            with_sim_global(|g| g.borrow_mut().freertos_parked = true);
                            // Only a callback can do anything before the
                            // unmask.
                            return RunReport {
                                more: next_event.is_some(),
                                next_wake: next_event,
                            };
                        }
                    }
                }
                let due = next_due(*sim_time);
                if io_waiting()
                    && poll_host_io(*sim_time, Some(due.map_or(limit, |d| d.min(limit))))
                {
                    continue;
                }
                if ended() {
                    return DONE;
                }
                // Nothing can run before the next wake-up.
                match due {
                    Some(due) if due <= limit => {
                        advance_and_dispatch(sim_time, due);
                        slices = 0;
                    }
                    due => {
                        // Keep firmware time in step with the World.
                        if limit > *sim_time {
                            advance_ticks(sim_time, limit - *sim_time);
                        }
                        with_sim_global(|g| g.borrow_mut().freertos_parked = true);
                        let more = due.is_some() || io_waiting();
                        return RunReport {
                            more,
                            next_wake: due.or_else(|| more.then_some(limit + 1)),
                        };
                    }
                }
            }
            Some(YieldReason::BudgetExceeded) => {
                if let Some(report) = charge_tick(sim_time, limit) {
                    return report;
                }
                slices = 0;
            }
            _ => {
                switch_after_task_yield(idx);
                if slices >= SLICES_PER_TICK {
                    if let Some(report) = charge_tick(sim_time, limit) {
                        return report;
                    }
                    slices = 0;
                }
            }
        }
    }
}

/// A task used a tick's worth of CPU: advance one tick (a tick interrupt),
/// unless that would pass `limit`, in which case report that the machine
/// must run again at the next tick.
fn charge_tick(sim_time: &mut Tick, limit: Tick) -> Option<RunReport> {
    // Callbacks due at the current tick run at it, before the tick: an
    // exhausted budget defers task work, never a callback's deadline.
    dispatch_events(*sim_time);
    // A callback may have ended the scheduler: no kernel call after that.
    if ended() {
        return Some(RunReport::DONE);
    }
    deliver_pending_irqs(*sim_time);
    // So may an ISR.
    if ended() {
        return Some(RunReport::DONE);
    }
    if *sim_time >= limit {
        // The tick interrupt cannot happen before the World reaches the
        // next tick: charge it at the start of the next step.  Until then
        // the task keeps the CPU unless an ISR asked for a switch.
        with_sim_global(|g| g.borrow_mut().freertos_tick_owed = true);
        switch_if_requested(false);
        return Some(RunReport {
            more: true,
            next_wake: Some(*sim_time + 1),
        });
    }
    let tick_switch = advance_ticks(sim_time, 1);
    set_sim_now(*sim_time);
    dispatch_events(*sim_time);
    if ended() {
        return Some(RunReport::DONE);
    }
    deliver_pending_irqs(*sim_time);
    if ended() {
        return Some(RunReport::DONE);
    }
    switch_if_requested(tick_switch);
    None
}

/// A switch FreeRTOS requested from scheduler context (`portYIELD` while no
/// task runs: an ISR's `portYIELD_FROM_ISR()`, or `xTaskCreate()` of a
/// higher-priority task) that interrupts, now unmasked, let the engine
/// perform.
fn yield_requested() -> bool {
    !crate::is_critical_locked() && guest_runtime::interrupt_state().yield_pending
}

/// After a tick interrupt charged for an exhausted budget: switch only if
/// the kernel's tick handler (`xTaskIncrementTick()`, for a woken
/// higher-priority task or for time slicing when enabled) or an ISR
/// (`portYIELD_FROM_ISR()`) requested it.  Otherwise the interrupted task
/// keeps running, as on hardware.
fn switch_if_requested(tick_switch: bool) {
    let isr_switch = guest_runtime::interrupt_state().yield_pending;
    if tick_switch || isr_switch {
        switch_context_after_isrs();
    }
}

/// Advance virtual time to the next wake-up (delayed task or peripheral
/// event) and let FreeRTOS pick the next task.  Returns `false` if nothing
/// is scheduled and no host I/O can wake a task.
fn wait_for_next_event(sim_time: &mut Tick) -> bool {
    let target = next_due(*sim_time);
    let io_waiting = io_waiting();
    if io_waiting && poll_host_io(*sim_time, target) {
        // A task's descriptor is ready now: run it before time moves.
        set_sim_now(*sim_time);
        return true;
    }
    if ended() {
        return false;
    }

    match target {
        Some(target) => {
            advance_and_dispatch(sim_time, target);
            if ended() {
                return false;
            }
        }
        None if io_waiting => {
            switch_context_after_isrs();
            set_sim_now(*sim_time);
        }
        None => return false,
    }
    true
}

/// Move virtual time on by `count` ticks and run the FreeRTOS tick
/// interrupts for them.  Returns whether the tick handler requested a
/// context switch.
///
/// While interrupts are masked the tick interrupt is masked too: time
/// moves, but the kernel does not count the ticks yet (no delayed task
/// wakes, no time slice ends).  They are kept in
/// `SimGlobal::freertos_masked_ticks` and serviced at once when interrupts
/// are unmasked ([`service_masked_ticks`] in a task, here or at the next
/// step's entry otherwise).
fn advance_ticks(sim_time: &mut Tick, count: u64) -> bool {
    *sim_time += count;
    set_sim_now(*sim_time);
    if crate::is_critical_locked() {
        with_sim_global(|g| g.borrow_mut().freertos_masked_ticks += count);
        return false;
    }
    let masked = with_sim_global(|g| std::mem::take(&mut g.borrow_mut().freertos_masked_ticks));
    kernel_ticks(masked + count)
}

/// Run `count` FreeRTOS tick interrupts (`xTaskIncrementTick()`) without
/// moving virtual time.  Returns whether the tick handler requested a
/// context switch.
fn kernel_ticks(mut count: u64) -> bool {
    let mut switch = false;
    while count > 0 {
        let chunk = count.min(u64::from(u32::MAX)) as u32;
        // Safety: the machine's kernel is active; called from scheduler
        // context, or from a task as its pending tick interrupt.
        switch |= unsafe { sim_advance_ticks(chunk) } != 0;
        count -= u64::from(chunk);
    }
    switch
}

/// Service the tick interrupts that were held off while interrupts were
/// masked, now that they are unmasked (`portENABLE_INTERRUPTS()`, the end
/// of a critical section) — at once, wherever the unmask happens, so the
/// kernel's tick count is current for whatever runs next.
///
/// In a task this is the pending tick interrupt firing on its stack.  In
/// scheduler context (a peripheral callback that unmasks) or from host code
/// between steps it runs right there.  A switch the kernel requests (a
/// woken higher-priority task, a time slice) is latched in `yield_pending`:
/// the task performs it as it returns from the unmask; otherwise the engine
/// performs it once the callback returns, or at the next step, which the
/// machine is woken for.
pub(crate) fn service_masked_ticks() {
    if crate::is_critical_locked() {
        return;
    }
    let count = with_sim_global(|g| {
        g.try_borrow_mut()
            .map(|mut g| std::mem::take(&mut g.freertos_masked_ticks))
            .unwrap_or(0)
    });
    if count > 0 && kernel_ticks(count) {
        guest_runtime::update_interrupt_state(|s| s.yield_pending = true);
    }
    if !has_active_fiber() && (count > 0 || guest_runtime::interrupt_state().yield_pending) {
        // Between steps: the work the ticks or the unmask released wakes
        // the machine (inside a step, the step's own report replaces this).
        with_sim_global(|g| {
            if let Ok(mut g) = g.try_borrow_mut() {
                g.note_new_task();
            }
        });
    }
}

/// At a step's entry, or after a slice: service tick interrupts held off
/// while interrupts were masked if they are unmasked now (host code
/// unmasked them between steps, or a task faulted masked).  A switch the
/// kernel requests is latched for the step to perform.
fn catch_up_masked_ticks() {
    if crate::is_critical_locked() {
        return;
    }
    let count = with_sim_global(|g| std::mem::take(&mut g.borrow_mut().freertos_masked_ticks));
    if count > 0 && kernel_ticks(count) {
        guest_runtime::update_interrupt_state(|s| s.yield_pending = true);
    }
}

/// `configTICK_RATE_HZ` of the linked FreeRTOS build.
pub fn tick_rate_hz() -> u32 {
    // Safety: returns a compile-time constant.
    unsafe { sim_freertos_tick_rate_hz() }
}
