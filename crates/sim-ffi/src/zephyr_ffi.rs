//! Zephyr-specific C ABI exports.

use sim_core::time::Tick;
use sim_fiber::{yield_reason::YieldReason, Fiber};
use std::sync::atomic::Ordering;

use crate::{
    deliver_pending_irqs, dispatch_events, next_event_deadline, run_one_scheduler_cycle,
    set_sim_now, suspend_active_fiber, with_sim_global, CURRENT_TASK_ID, TL_TRACE,
    ZEPHYR_SCHEDULER_TICK_STATE,
};

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
    with_sim_global(|global| {
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
/// or `sim_set_current_task_by_id` — Zephyr doesn concept in the same way, and uses TCB pointers instead of task IDs.
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
        let task_idx: Option<usize> = with_sim_global(|global| {
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

                let task_id = with_sim_global(|global| {
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
                let (yield_reason, panicked) = with_sim_global(|global| {
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
                with_sim_global(|global| {
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
                        with_sim_global(|global| {
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
                        let any_alive = with_sim_global(|global| {
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
                            with_sim_global(|global| {
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

/// Advance the Zephyr scheduler by one cycle and return.
///
/// This is the Zephyr equivalent of [`sim_scheduler_tick`].  Each call
/// executes exactly one scheduling decision: either resume a runnable
/// Zephyr fiber (which runs until it yields, blocks, or exits) OR advance
/// virtual time to the next event boundary and wake any sleepers.
///
/// Returns 1 if the simulation has more work to do (runnable or sleeping
/// tasks remain), or 0 if the simulation is complete (no runnable tasks
/// and no sleeping tasks and no I/O progress).
///
/// # Safety
///
/// Must be called from the main scheduler context (not within a fiber).
/// The caller is responsible for calling this repeatedly until it returns 0.
///
/// # Differences from sim_scheduler_tick
///
/// - No FreeRTOS-specific setup (no `sim_exit_critical` or
///   `sim_bridge_create_pending_fibers`).
/// - Uses `sim_zephyr_set_current_thread` to inform the C side which
///   TCB is current (matching Zephyr's TCB-pointer model).
#[no_mangle]
pub unsafe extern "C" fn sim_zephyr_scheduler_tick() -> u32 {
    ZEPHYR_SCHEDULER_TICK_STATE.with(|state| {
        let mut s = state.borrow_mut();

        // One-time setup on first call from this thread.
        if !s.initialized {
            s.initialized = true;
            s.sim_time = 0;
        }

        let mut sim_time = s.sim_time;
        let more = run_one_scheduler_cycle(&mut sim_time);
        s.sim_time = sim_time;

        // Flush thread-local trace into the active SimGlobal's trace sink.
        crate::flush_trace();

        if more {
            1
        } else {
            0
        }
    })
}
