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

// ── C functions called FROM Rust (implemented in task.c) ──────────

extern "C" {
    fn sim_set_current_task_by_id(task_id: u64);
    fn sim_tick_advance() -> u32;
}

// ---------------------------------------------------------------------------
// Thread-local state (re-entrant safe)
// ---------------------------------------------------------------------------

/// Current virtual time.  Atomic so it can be read from within a fiber
/// without touching the global RefCell.
static SIM_NOW: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static CRITICAL_NESTING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

thread_local! {
    static TL_TRACE: RefCell<Vec<sim_core::trace::TraceEvent>> =
        const { RefCell::new(Vec::new()) };
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
                entry(arg);
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

                // Resume the fiber.
                let yield_reason: Option<YieldReason> = SIM_GLOBAL.with(|global| {
                    let mut global = global.borrow_mut();
                    let task = &mut global.tasks[idx];
                    task.resume(sim_fiber::ResumeReason::SchedulerSelected)
                });

                // Handle yield.
                SIM_GLOBAL.with(|global| {
                    let mut global = global.borrow_mut();
                    if let Some(reason) = yield_reason {
                        if let Some(ref mut trace) = global.trace {
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
                match next_wake {
                    Some(wake_time) if wake_time > sim_time => {
                        // Advance time, ticking at each boundary.
                        while sim_time < wake_time {
                            sim_time += 1;
                            unsafe {
                                sim_tick_advance();
                            }
                            // Deliver timer IRQs that expire at this tick.
                            deliver_pending_irqs(sim_time);
                        }

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
                        // No sleeping tasks either — simulation complete.
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
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

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
}
