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

use crate::device_ffi::{deliver_pending_irqs, in_isr};
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
    fn sim_freertos_ticks_until_unblock() -> u32;
    fn sim_freertos_scheduler_running() -> u32;
    fn sim_freertos_timers_in_use() -> u32;
    fn sim_freertos_start_external();
    fn sim_freertos_retire_current();
    fn sim_port_task_returned();
    fn sim_advance_ticks(count: u32) -> u32;
    fn sim_freertos_tick_rate_hz() -> u32;
    fn vTaskDelay(ticks: u32);
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
        && !in_isr()
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
    if crate::is_critical_locked() || in_isr() || !has_active_fiber() {
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
        let waits = &mut g.borrow_mut().freertos_io_waits;
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

/// Run `vTaskSwitchContext()`: FreeRTOS selects the next task.
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
        global.tasks.push(fiber);
        global.unclaimed_freertos_tasks.push((id, entry as usize));

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
    with_sim_global(|g| g.borrow_mut().freertos_ended = true);
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
pub(crate) fn ensure_started() {
    let has_tasks = with_sim_global(|g| g.borrow().freertos);
    // Safety: scheduler context, machine kernel active.
    unsafe {
        let used = has_tasks || sim_freertos_timers_in_use() != 0;
        if used && sim_freertos_scheduler_running() == 0 {
            sim_freertos_start_external();
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

/// The task FreeRTOS selected, as `(index in the task table, fiber state)`.
fn current_task() -> Option<(usize, sim_fiber::TaskState)> {
    // Safety: scheduler context, machine kernel active.
    let handle = unsafe { sim_freertos_current_handle() };
    with_sim_global(|global| {
        let global = global.borrow();
        global
            .tasks
            .iter()
            .position(|t| t.id == handle && !t.is_terminated())
            .map(|idx| (idx, global.tasks[idx].state))
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
    if woken {
        switch_context();
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
/// wrap), a peripheral event, a virtual timer expiry or the arrival of a
/// pending IRQ.
fn next_due(sim_time: Tick) -> Option<Tick> {
    // Safety: scheduler context, machine kernel active.
    let until_unblock = unsafe { sim_freertos_ticks_until_unblock() };
    let wake = (until_unblock != u32::MAX).then(|| sim_time + u64::from(until_unblock.max(1)));
    [
        wake,
        next_event_deadline(),
        sim_devices::next_timer_expiry(),
        sim_devices::irq::with_irq(|c| c.next_arrival_after(sim_time)),
    ]
    .into_iter()
    .flatten()
    .min()
}

/// Advance to `target`, fire what is due there and let FreeRTOS reschedule.
fn advance_and_dispatch(sim_time: &mut Tick, target: Tick) {
    if target > *sim_time {
        advance_ticks(sim_time, target - *sim_time);
    }
    dispatch_events(*sim_time);
    deliver_pending_irqs(*sim_time);
    switch_context();
    set_sim_now(*sim_time);
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
    let Some((idx, _)) = current_task() else {
        return false;
    };

    let Some(reason) = run_slice(idx, *sim_time) else {
        return false;
    };

    match reason {
        Some(YieldReason::Idle) => {
            switch_context();
            if idle_is_current() {
                // Every application task is blocked.
                return wait_for_next_event(sim_time);
            }
        }
        Some(YieldReason::BudgetExceeded) => {
            // The task burnt a tick's worth of CPU: deliver a tick
            // interrupt so time moves and higher-priority tasks can preempt.
            advance_ticks(sim_time, 1);
            dispatch_events(*sim_time);
            deliver_pending_irqs(*sim_time);
            switch_context();
        }
        _ => switch_context(),
    }
    set_sim_now(*sim_time);
    true
}

/// Run the machine until every task is blocked past tick `limit`, or until
/// running further would move virtual time beyond `limit`.
///
/// Used by Worlds, which derive `limit` from their own clock: firmware time
/// never runs ahead of the World, and all work due at the current instant
/// happens in one call.
pub(crate) fn run_until(sim_time: &mut Tick, limit: Tick) -> RunReport {
    const DONE: RunReport = RunReport {
        more: false,
        next_wake: None,
    };
    let mut slices = 0u32;

    // IRQs raised between steps (World input) arrive at the World's
    // current instant, `limit`: work due before then happens first, and
    // the ISR and the tasks it wakes run at `limit`, not at the machine's
    // last firmware time.  (With interrupts masked they wait for the
    // unmask anyway.)
    if !crate::is_critical_locked() {
        sim_devices::irq::with_irq_mut(|c| c.stamp_arrivals(limit));
    }
    deliver_pending_irqs(*sim_time);

    loop {
        process_pending_deletions();
        if ended() {
            return DONE;
        }

        let parked = with_sim_global(|g| std::mem::take(&mut g.borrow_mut().freertos_parked));
        if parked {
            // Input delivered since the last call (a World event, an ISR)
            // may have readied a task.
            switch_context();
        }
        let Some((idx, _)) = current_task() else {
            return DONE;
        };

        let reason = if parked && idle_is_current() {
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
                switch_context();
                if !idle_is_current() {
                    continue;
                }
                let due = next_due(*sim_time);
                if io_waiting()
                    && poll_host_io(*sim_time, Some(due.map_or(limit, |d| d.min(limit))))
                {
                    continue;
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
                switch_context();
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
    if *sim_time >= limit {
        switch_context();
        return Some(RunReport {
            more: true,
            next_wake: Some(*sim_time + 1),
        });
    }
    advance_ticks(sim_time, 1);
    dispatch_events(*sim_time);
    deliver_pending_irqs(*sim_time);
    switch_context();
    set_sim_now(*sim_time);
    None
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

    match target {
        Some(target) => advance_and_dispatch(sim_time, target),
        None if io_waiting => {
            switch_context();
            set_sim_now(*sim_time);
        }
        None => return false,
    }
    true
}

/// Run `count` FreeRTOS tick interrupts and move virtual time with them.
fn advance_ticks(sim_time: &mut Tick, mut count: u64) {
    while count > 0 {
        let chunk = count.min(u64::from(u32::MAX)) as u32;
        *sim_time += u64::from(chunk);
        set_sim_now(*sim_time);
        // Safety: scheduler context, machine kernel active.
        unsafe { sim_advance_ticks(chunk) };
        count -= u64::from(chunk);
    }
}

/// `configTICK_RATE_HZ` of the linked FreeRTOS build.
pub fn tick_rate_hz() -> u32 {
    // Safety: returns a compile-time constant.
    unsafe { sim_freertos_tick_rate_hz() }
}
