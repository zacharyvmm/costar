//! FreeRTOS scheduling regressions.
//!
//! Each test boots one scenario from `c_firmware/tests/sched_regressions.c`
//! on its own `Simulator`, steps the scheduler the way a World does, and
//! checks the firmware's `sim_trace_u32` records.  The firmware only uses
//! plain FreeRTOS APIs: FreeRTOS, not the simulator, makes every
//! scheduling decision.

use sim_core::trace::TraceEvent;
use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;

extern "C" {
    fn costar_test_runtime_create_boot();
    fn costar_test_blocking_queue_boot();
    fn costar_test_software_timer_boot();
    fn costar_test_task_param_boot();
    fn costar_test_task_return_boot();
    fn costar_test_static_task_boot();
    fn costar_test_legacy_pattern_boot();
    fn costar_test_abi_delay_boot();
    fn costar_test_masked_fault_boot();
    fn costar_test_reserved_tls_boot();
    fn costar_test_busy_no_slicing_boot();
    fn costar_test_legacy_reverse_boot();
    fn vTaskStartScheduler();
    #[cfg(unix)]
    fn costar_test_io_wait_boot(recv_fd: i32, send_fd: i32);
    #[cfg(unix)]
    fn costar_test_io_delete_boot(fd: i32, reuse: i32);
    fn costar_test_timer_isr_boot();
    fn costar_test_timer_isr_ack_boot();
    fn costar_test_isr_preemption_boot();
    fn costar_test_external_irq_boot();
    fn costar_test_isr_budget_boot();
}

struct Run {
    /// `(time, label, value)` for every `sim_trace_u32` call.
    records: Vec<(u64, &'static str, u32)>,
    events: Vec<TraceEvent>,
}

impl Run {
    fn labels(&self, label: &str) -> Vec<(u64, u32)> {
        self.records
            .iter()
            .filter(|(_, l, _)| *l == label)
            .map(|&(at, _, value)| (at, value))
            .collect()
    }

    fn index_of(&self, label: &str, value: u32) -> usize {
        self.records
            .iter()
            .position(|&(_, l, v)| l == label && v == value)
            .unwrap_or_else(|| panic!("no {label}={value} in {:?}", self.records))
    }

    fn assert_no_fatal(&self) {
        let fatals: Vec<_> = self
            .events
            .iter()
            .filter(|e| matches!(e, TraceEvent::Fatal { .. }))
            .collect();
        assert!(fatals.is_empty(), "fatal trace events: {fatals:?}");
    }

    fn tasks_created(&self) -> Vec<&'static str> {
        self.events
            .iter()
            .filter_map(|e| match e {
                TraceEvent::TaskCreated { name, .. } => Some(*name),
                _ => None,
            })
            .collect()
    }
}

/// Boot a scenario and step the scheduler until it goes quiescent, virtual
/// time passes `until`, or `max_steps` is reached.
fn run(boot: unsafe extern "C" fn(), until: u64, max_steps: usize) -> Run {
    run_with(|| {}, boot, until, max_steps)
}

/// Like [`run`], with `setup` run on the active simulator before boot
/// (e.g. to create virtual devices).
fn run_with(
    setup: impl FnOnce(),
    boot: unsafe extern "C" fn(),
    until: u64,
    max_steps: usize,
) -> Run {
    let mut sim = Simulator::new(SimConfig::default());
    sim.enable_owned_devices();
    let global = sim.sim_global.clone();
    {
        let _active = sim.activate();
        setup();
        unsafe { boot() };
        for _ in 0..max_steps {
            if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
                break;
            }
            if global.borrow().scheduler_sim_time > until {
                break;
            }
        }
    }
    let global = global.borrow();
    let events = global.trace.as_ref().unwrap().events.clone();
    let records = events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 { at, label, value } => Some((*at, *label, *value)),
            _ => None,
        })
        .collect();
    Run { records, events }
}

#[test]
fn task_created_at_runtime_runs() {
    let r = run(costar_test_runtime_create_boot, 100, 1_000);
    r.assert_no_fatal();
    assert_eq!(r.labels("parent_created_child"), vec![(2, 1)]);
    assert_eq!(r.labels("child_ran"), vec![(2, 1)]);
    assert!(r.index_of("parent_created_child", 1) < r.index_of("child_ran", 1));
}

#[test]
fn blocking_receive_wakes_on_send_and_preempts_sender() {
    let r = run(costar_test_blocking_queue_boot, 100, 1_000);
    r.assert_no_fatal();
    let rx: Vec<_> = r.labels("rx");
    assert_eq!(rx, vec![(0, 1), (1, 2), (2, 3), (3, 4), (4, 5)]);
    // The priority-3 consumer runs inside xQueueSend(), before the
    // priority-1 producer records "sent".
    for v in 1..=5 {
        assert!(r.index_of("rx", v) < r.index_of("sent", v), "item {v}");
    }
}

#[test]
fn software_timer_callbacks_fire() {
    let r = run(costar_test_software_timer_boot, 55, 10_000);
    r.assert_no_fatal();
    assert_eq!(
        r.labels("timer_fired"),
        vec![(10, 1), (20, 2), (30, 3), (40, 4), (50, 5)]
    );
}

#[test]
fn task_receives_its_parameter() {
    let r = run(costar_test_task_param_boot, 100, 1_000);
    r.assert_no_fatal();
    assert_eq!(r.labels("param_ok"), vec![(0, 1)]);
    assert_eq!(r.labels("param_value"), vec![(0, 0xC0FFEE)]);
}

#[test]
fn returning_task_is_deleted_and_others_keep_running() {
    let r = run(costar_test_task_return_boot, 100, 1_000);
    r.assert_no_fatal();
    assert_eq!(r.labels("returning"), vec![(0, 1)]);
    assert_eq!(r.labels("task_returned"), vec![(0, 1)]);
    assert_eq!(r.labels("survivor_ran"), vec![(3, 1)]);
}

#[test]
fn static_task_fits_its_buffer() {
    let r = run(costar_test_static_task_boot, 100, 1_000);
    r.assert_no_fatal();
    assert_eq!(r.labels("static_ran"), vec![(0, 1)]);
    assert_eq!(r.labels("static_guard_intact"), vec![(0, 1)]);
}

#[test]
fn legacy_double_creation_yields_one_task() {
    let r = run(costar_test_legacy_pattern_boot, 100, 1_000);
    r.assert_no_fatal();
    assert_eq!(r.labels("legacy_ran"), vec![(1, 1)]);
    let legacy = r.tasks_created().iter().filter(|n| **n == "legacy").count();
    assert_eq!(legacy, 1, "tasks created: {:?}", r.tasks_created());
}

#[test]
fn simulator_delay_abi_blocks_the_freertos_task() {
    let r = run(costar_test_abi_delay_boot, 20, 1_000);
    r.assert_no_fatal();
    assert_eq!(r.labels("freertos_delay_done"), vec![(5, 5)]);
    // While the delayer waits in sim_task_delay_until(), FreeRTOS runs the
    // lower-priority task.
    assert_eq!(r.labels("background_ran"), vec![(8, 8)]);
    assert_eq!(r.labels("abi_delay_done"), vec![(12, 12)]);
}

#[test]
fn fault_inside_critical_section_leaves_interrupts_usable() {
    let mut sim = Simulator::new(SimConfig::default());
    let global = sim.sim_global.clone();
    let _active = sim.activate();
    unsafe { costar_test_masked_fault_boot() };
    for _ in 0..1_000 {
        if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
            break;
        }
    }
    assert!(!sim_ffi::is_critical_locked());
    let global = global.borrow();
    let events = &global.trace.as_ref().unwrap().events;
    assert!(events.iter().any(|e| matches!(e, TraceEvent::Fatal { .. })));
    // With the mask left set, vTaskDelay(1) could not switch away and
    // returned at tick 0.
    let after: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 {
                at,
                label: "after_fault_ran",
                value,
            } => Some((*at, *value)),
            _ => None,
        })
        .collect();
    assert_eq!(after, vec![(1, 1)]);
}

#[test]
fn firmware_cannot_overwrite_the_reserved_tls_slot() {
    let r = run(costar_test_reserved_tls_boot, 10, 1_000);
    assert_eq!(r.labels("tls_slot0_ok"), vec![(0, 1)]);
    assert!(r
        .events
        .iter()
        .any(|e| matches!(e, TraceEvent::Fatal { .. })));
    assert!(r.labels("tls_reserved_written").is_empty());
    assert_eq!(r.labels("tls_bystander_ran"), vec![(1, 1)]);
}

#[test]
fn interrupt_mask_belongs_to_its_simulator() {
    use sim_ffi::freertos::{sim_disable_interrupts, sim_enable_interrupts};
    use sim_ffi::{is_critical_locked, sim_enter_critical, sim_exit_critical};

    let mut a = Simulator::new(SimConfig::default());
    let mut b = Simulator::new(SimConfig::default());
    {
        let _a = a.activate();
        sim_disable_interrupts();
        unsafe { sim_enter_critical() };
    }
    {
        let _b = b.activate();
        assert!(!is_critical_locked(), "A's mask leaked into B");
        unsafe { sim_enter_critical() };
    }
    {
        let _a = a.activate();
        unsafe { sim_exit_critical() };
        assert!(
            is_critical_locked(),
            "A's portDISABLE_INTERRUPTS() was lost"
        );
        sim_enable_interrupts();
        assert!(!is_critical_locked());
    }
    {
        let _b = b.activate();
        assert!(is_critical_locked(), "B's critical section was lost");
        unsafe { sim_exit_critical() };
        assert!(!is_critical_locked());
    }
    assert!(!is_critical_locked(), "standalone state changed");
}

#[test]
#[cfg(unix)]
fn host_io_wait_lets_lower_priority_tasks_run() {
    use std::os::fd::AsRawFd;

    // Bounded (World) and unbounded (standalone) stepping.
    for limit in [Some(0), None] {
        let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let mut sim = Simulator::new(SimConfig::default());
        sim.enable_owned_devices();
        sim.enable_owned_network();
        let global = sim.sim_global.clone();
        let _active = sim.activate();
        assert_eq!(
            unsafe { sim_ffi::net_ffi::sim_host_register_fd(reader.as_raw_fd()) },
            0
        );
        unsafe { costar_test_io_wait_boot(reader.as_raw_fd(), writer.as_raw_fd()) };
        sim.set_scheduler_limit(limit);
        for _ in 0..100 {
            if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
                break;
            }
        }
        sim_ffi::net_ffi::sim_host_deregister_fd(reader.as_raw_fd());

        let global = global.borrow();
        let records: Vec<_> = global
            .trace
            .as_ref()
            .unwrap()
            .events
            .iter()
            .filter_map(|e| match e {
                TraceEvent::UserU32 { label, value, .. } if label.starts_with("io_") => {
                    Some((*label, *value))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            records,
            vec![("io_sent", 1), ("io_received", u32::from(b'x'))],
            "limit {limit:?}"
        );
    }
}

#[test]
#[cfg(unix)]
fn deleting_an_io_waiter_cancels_its_wait() {
    use std::io::Write;
    use std::os::fd::AsRawFd;

    for reuse in [false, true] {
        for limit in [Some(0), None] {
            let case = format!("reuse {reuse}, limit {limit:?}");
            let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
            reader.set_nonblocking(true).unwrap();
            let mut sim = Simulator::new(SimConfig::default());
            sim.enable_owned_devices();
            sim.enable_owned_network();
            let global = sim.sim_global.clone();
            let _active = sim.activate();
            assert_eq!(
                unsafe { sim_ffi::net_ffi::sim_host_register_fd(reader.as_raw_fd()) },
                0
            );
            unsafe { costar_test_io_delete_boot(reader.as_raw_fd(), i32::from(reuse)) };
            sim.set_scheduler_limit(limit);
            // No input: once the waiter is deleted nothing is left to do.
            let quiescent = (0..10).any(|_| unsafe { sim_ffi::sim_scheduler_tick() } == 0);
            assert!(quiescent, "{case}: the deleted wait kept the machine alive");
            // Readiness after the deletion must not resume anything.
            writer.write_all(b"x").unwrap();
            assert_eq!(unsafe { sim_ffi::sim_scheduler_tick() }, 0, "{case}");
            sim_ffi::net_ffi::sim_host_deregister_fd(reader.as_raw_fd());

            let global = global.borrow();
            let events = &global.trace.as_ref().unwrap().events;
            let io_resumes = events
                .iter()
                .filter(|e| {
                    matches!(
                        e,
                        TraceEvent::TaskResume {
                            reason: "io_ready",
                            ..
                        }
                    )
                })
                .count();
            assert_eq!(io_resumes, 0, "{case}");
            let mut records: Vec<_> = events
                .iter()
                .filter_map(|e| match e {
                    TraceEvent::UserU32 { label, value, .. } if label.starts_with("io_") => {
                        Some((*label, *value))
                    }
                    _ => None,
                })
                .collect();
            // Whether the allocator reused the TCB is not up to the test.
            records.retain(|&(label, _)| label != "io_tcb_reused");
            let mut expected = vec![("io_waiter_deleted", 1)];
            if reuse {
                expected.push(("io_reuser_started", 1));
            }
            assert_eq!(records, expected, "{case}");
        }
    }
}

/// Native Rust task body for the mixed-scheduling tests: records when it
/// wakes from each kind of sleep.
fn native_sleeper(ctx: sim_ffi::TaskContext) {
    let trace = |label: &'static [u8]| unsafe {
        sim_ffi::sim_trace_u32(label.as_ptr().cast(), ctx.now() as u32)
    };
    trace(b"native_started\0");
    ctx.sleep_for(3);
    trace(b"native_slept\0");
    ctx.yield_now();
    ctx.sleep_until(10);
    trace(b"native_done\0");
}

/// Boot `costar_test_abi_delay_boot` with a native Rust task spawned
/// before the firmware boots or once FreeRTOS runs (after the first
/// scheduler step at tick 0), and
/// step it as a standalone Simulator (`world = false`) or the way a World
/// does (bounded by `scheduler_limit`).
fn run_mixed(spawn_after_boot: bool, world: bool) -> Run {
    let mut sim = Simulator::new(SimConfig::default());
    let global = sim.sim_global.clone();
    {
        let _active = sim.activate();
        if !spawn_after_boot {
            sim_ffi::spawn_rust_task("native", 3, 4096, native_sleeper);
        }
        unsafe { costar_test_abi_delay_boot() };
        for step in 0..1_000u64 {
            if spawn_after_boot && step == 1 {
                sim_ffi::spawn_rust_task("native", 3, 4096, native_sleeper);
            }
            if world {
                global.borrow_mut().scheduler_limit = Some(step);
            }
            let more = unsafe { sim_ffi::sim_scheduler_tick() } != 0;
            if (!more && !world) || global.borrow().scheduler_sim_time > 20 {
                break;
            }
        }
    }
    let global = global.borrow();
    let events = global.trace.as_ref().unwrap().events.clone();
    let records = events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 { at, label, value } => Some((*at, *label, *value)),
            _ => None,
        })
        .collect();
    Run { records, events }
}

#[test]
fn native_rust_tasks_run_alongside_freertos_tasks() {
    for spawn_after_boot in [false, true] {
        for world in [false, true] {
            let r = run_mixed(spawn_after_boot, world);
            let case = format!("spawn_after_boot={spawn_after_boot} world={world}");
            r.assert_no_fatal();
            // The native task sleeps in FreeRTOS's delayed list: it wakes
            // on time and the firmware's tasks run meanwhile.
            assert_eq!(r.labels("native_started"), vec![(0, 0)], "{case}");
            assert_eq!(r.labels("native_slept"), vec![(3, 3)], "{case}");
            assert_eq!(r.labels("native_done"), vec![(10, 10)], "{case}");
            assert_eq!(r.labels("freertos_delay_done"), vec![(5, 5)], "{case}");
            assert_eq!(r.labels("background_ran"), vec![(8, 8)], "{case}");
            assert_eq!(r.labels("abi_delay_done"), vec![(12, 12)], "{case}");
        }
    }
}

/// Records of a FreeRTOS machine whose firmware `boot`s, stepped
/// standalone (`world = false`) or World-style, until virtual time passes
/// `until`.  `setup` runs on the active simulator before boot.
fn run_stepped(setup: impl FnOnce(), boot: unsafe extern "C" fn(), world: bool, until: u64) -> Run {
    let mut sim = Simulator::new(SimConfig::default());
    let global = sim.sim_global.clone();
    {
        let _active = sim.activate();
        setup();
        unsafe { boot() };
        for step in 0..10_000u64 {
            if world {
                global.borrow_mut().scheduler_limit = Some(step);
            }
            let more = unsafe { sim_ffi::sim_scheduler_tick() } != 0;
            if (!more && !world) || global.borrow().scheduler_sim_time > until {
                break;
            }
        }
    }
    let global = global.borrow();
    let events = global.trace.as_ref().unwrap().events.clone();
    let records = events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 { at, label, value } => Some((*at, *label, *value)),
            _ => None,
        })
        .collect();
    Run { records, events }
}

#[test]
fn native_task_panic_is_isolated_on_a_freertos_machine() {
    for world in [false, true] {
        let r = run_stepped(
            || {
                sim_ffi::spawn_rust_task("panicker", 3, 4096, |_ctx| {
                    panic!("deliberate panic in a native task");
                });
            },
            costar_test_abi_delay_boot,
            world,
            20,
        );
        let case = format!("world={world}");
        assert!(
            r.events.iter().any(|e| matches!(
                e,
                TraceEvent::Fatal {
                    code: sim_core::error::SimErrorCode::PanicCrossedCAbi,
                    ..
                }
            )),
            "{case}"
        );
        // The firmware's tasks keep running on time.
        assert_eq!(r.labels("freertos_delay_done"), vec![(5, 5)], "{case}");
        assert_eq!(r.labels("background_ran"), vec![(8, 8)], "{case}");
        assert_eq!(r.labels("abi_delay_done"), vec![(12, 12)], "{case}");
    }
}

#[test]
fn native_yield_inside_a_critical_section_is_deferred() {
    fn trace(label: &'static [u8]) {
        unsafe { sim_ffi::sim_trace_u32(label.as_ptr().cast(), 1) }
    }
    for world in [false, true] {
        let r = run_stepped(
            || {
                sim_ffi::spawn_rust_task("masker", 3, 4096, |ctx| {
                    unsafe { sim_ffi::sim_enter_critical() };
                    ctx.yield_now(); // pended: interrupts are masked
                    trace(b"masker_still_running\0");
                    unsafe { sim_ffi::sim_exit_critical() }; // switch happens here
                    trace(b"masker_after_unmask\0");
                });
                sim_ffi::spawn_rust_task("observer", 3, 4096, |_ctx| {
                    trace(b"observer_ran\0");
                });
            },
            costar_test_abi_delay_boot,
            world,
            20,
        );
        r.assert_no_fatal();
        let order: Vec<_> = r
            .records
            .iter()
            .map(|&(_, l, _)| l)
            .filter(|l| l.starts_with("masker") || l.starts_with("observer"))
            .collect();
        assert_eq!(
            order,
            vec![
                "masker_still_running",
                "observer_ran",
                "masker_after_unmask"
            ],
            "world={world}"
        );
    }
}

#[test]
fn freertos_boots_after_native_only_steps_and_keeps_the_clock() {
    for world in [false, true] {
        let mut sim = Simulator::new(SimConfig::default());
        let global = sim.sim_global.clone();
        {
            let _active = sim.activate();
            sim_ffi::spawn_rust_task("native", 3, 4096, |ctx| {
                ctx.sleep_until(3);
                unsafe { sim_ffi::sim_trace_u32(c"native_woke".as_ptr(), 1) };
                ctx.sleep_until(30);
            });
            // Native-only steps until virtual time reaches 3.
            for step in 0..100u64 {
                if world {
                    global.borrow_mut().scheduler_limit = Some(step);
                }
                unsafe { sim_ffi::sim_scheduler_tick() };
                if global.borrow().scheduler_sim_time >= 3 {
                    break;
                }
            }
            assert_eq!(global.borrow().scheduler_sim_time, 3, "world={world}");
            // Now the firmware boots without vTaskStartScheduler().
            unsafe { costar_test_abi_delay_boot() };
            for step in 3..1_000u64 {
                if world {
                    global.borrow_mut().scheduler_limit = Some(step);
                }
                let more = unsafe { sim_ffi::sim_scheduler_tick() } != 0;
                if (!more && !world) || global.borrow().scheduler_sim_time > 20 {
                    break;
                }
            }
        }
        let global = global.borrow();
        let records: Vec<(&str, u64, u32)> = global
            .trace
            .as_ref()
            .unwrap()
            .events
            .iter()
            .filter_map(|e| match e {
                TraceEvent::UserU32 { at, label, value } => Some((*label, *at, *value)),
                _ => None,
            })
            .collect();
        let at = |label| -> Vec<(u64, u32)> {
            records
                .iter()
                .filter(|&&(l, _, _)| l == label)
                .map(|&(_, at, v)| (at, v))
                .collect()
        };
        let case = format!("world={world}");
        assert_eq!(at("native_woke"), vec![(3, 1)], "{case}");
        // FreeRTOS's tick count starts at the virtual time it boots at (3):
        // vTaskDelay(5) ends at 8 and xTaskGetTickCount() agrees.
        assert_eq!(at("freertos_delay_done"), vec![(8, 8)], "{case}");
        assert_eq!(at("background_ran"), vec![(11, 11)], "{case}");
        assert_eq!(at("abi_delay_done"), vec![(12, 12)], "{case}");
        assert!(
            !records.iter().any(|&(l, _, _)| l == "assert_failed_line"),
            "{case}: {records:?}"
        );
    }
}

#[test]
fn budget_ticks_do_not_time_slice_with_time_slicing_disabled() {
    for world in [false, true] {
        let r = run_stepped(|| {}, costar_test_busy_no_slicing_boot, world, 20);
        r.assert_no_fatal();
        let ran: Vec<_> = r
            .records
            .iter()
            .map(|&(_, l, _)| l)
            .filter(|l| l.starts_with("busy_"))
            .collect();
        // configUSE_TIME_SLICING = 0: the task selected first keeps the CPU.
        assert_eq!(ran, vec!["busy_b"], "world={world}");
    }
}

/// `UserU32` records with a label starting with `prefix`, as
/// `(label, time, value)`.
fn records_with_prefix(r: &Run, prefix: &str) -> Vec<(&'static str, u64, u32)> {
    r.records
        .iter()
        .filter(|(_, l, _)| l.starts_with(prefix))
        .map(|&(at, l, v)| (l, at, v))
        .collect()
}

#[test]
fn adopting_a_lower_priority_native_task_does_not_rotate_busy_peers() {
    for world in [false, true] {
        let mut sim = Simulator::new(SimConfig::default());
        let global = sim.sim_global.clone();
        {
            let _active = sim.activate();
            unsafe { costar_test_busy_no_slicing_boot() };
            if world {
                global.borrow_mut().scheduler_limit = Some(0);
            }
            unsafe { sim_ffi::sim_scheduler_tick() };
            sim_ffi::spawn_rust_task("low", 1, 4096, |_| {});
            for step in 1..5 {
                if world {
                    global.borrow_mut().scheduler_limit = Some(step);
                }
                unsafe { sim_ffi::sim_scheduler_tick() };
            }
        }
        let busy: Vec<_> = global
            .borrow()
            .trace
            .as_ref()
            .unwrap()
            .events
            .iter()
            .filter_map(|e| match e {
                TraceEvent::UserU32 { label, .. } if label.starts_with("busy_") => Some(*label),
                _ => None,
            })
            .collect();
        assert_eq!(busy, vec!["busy_b"], "world={world}");
    }
}

#[test]
fn explicit_scheduler_start_after_native_steps_keeps_the_clock() {
    let mut sim = Simulator::new(SimConfig::default());
    let global = sim.sim_global.clone();
    {
        let _active = sim.activate();
        sim_ffi::spawn_rust_task("native", 3, 4096, |ctx| {
            ctx.sleep_until(3);
            ctx.sleep_until(30);
        });
        for _ in 0..100 {
            unsafe { sim_ffi::sim_scheduler_tick() };
            if global.borrow().scheduler_sim_time >= 3 {
                break;
            }
        }
        assert_eq!(global.borrow().scheduler_sim_time, 3);
        // The firmware starts the scheduler itself this time.
        unsafe {
            costar_test_abi_delay_boot();
            vTaskStartScheduler();
        }
        for _ in 0..100 {
            if unsafe { sim_ffi::sim_scheduler_tick() } == 0
                || global.borrow().scheduler_sim_time > 20
            {
                break;
            }
        }
    }
    let global = global.borrow();
    let done: Vec<_> = global
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 {
                at,
                label: "freertos_delay_done",
                value,
            } => Some((*at, *value)),
            _ => None,
        })
        .collect();
    // vTaskDelay(5) from tick 3; xTaskGetTickCount() agrees with the clock.
    assert_eq!(done, vec![(8, 8)]);
}

#[test]
fn budget_tick_at_the_world_limit_is_charged_before_the_task_resumes() {
    fn trace(label: &std::ffi::CStr, ctx: &sim_ffi::TaskContext) {
        unsafe { sim_ffi::sim_trace_u32(label.as_ptr(), ctx.now() as u32) }
    }
    for world in [false, true] {
        let r = run_stepped(
            || {
                sim_ffi::spawn_rust_task("high", 6, 4096, |ctx| {
                    ctx.sleep_for(1);
                    trace(c"probe_high_woke", &ctx);
                });
                sim_ffi::spawn_rust_task("low", 5, 4096, |ctx| {
                    unsafe {
                        sim_ffi::sim_budget_set_limit(1);
                        sim_ffi::sim_budget_poll(std::ptr::null(), 1);
                    }
                    trace(c"probe_low_after_budget", &ctx);
                });
            },
            costar_test_abi_delay_boot,
            world,
            20,
        );
        r.assert_no_fatal();
        // The budget stands for the rest of tick 0: the low task resumes at
        // tick 1, after the high task that tick woke.
        assert_eq!(
            records_with_prefix(&r, "probe_"),
            vec![("probe_high_woke", 1, 1), ("probe_low_after_budget", 1, 1)],
            "world={world}"
        );
    }
}

unsafe extern "C" fn abi_only_task(_: *mut std::ffi::c_void) {
    sim_ffi::sim_trace_u32(c"abi_only_ran".as_ptr(), 1);
}

#[test]
fn task_created_with_sim_create_task_runs_on_a_freertos_machine() {
    for before_boot in [true, false] {
        for world in [false, true] {
            let spawn = || unsafe {
                sim_ffi::sim_create_task(
                    c"abi_only".as_ptr(),
                    Some(abi_only_task),
                    std::ptr::null_mut(),
                    128,
                    4,
                )
            };
            let r = if before_boot {
                run_stepped(
                    || {
                        spawn();
                    },
                    costar_test_abi_delay_boot,
                    world,
                    20,
                )
            } else {
                unsafe extern "C" fn boot_then_spawn() {
                    costar_test_abi_delay_boot();
                    sim_ffi::sim_create_task(
                        c"abi_only".as_ptr(),
                        Some(abi_only_task),
                        std::ptr::null_mut(),
                        128,
                        4,
                    );
                }
                run_stepped(|| {}, boot_then_spawn, world, 20)
            };
            let case = format!("before_boot={before_boot} world={world}");
            r.assert_no_fatal();
            assert_eq!(r.labels("abi_only_ran"), vec![(0, 1)], "{case}");
            // The firmware's own tasks are unaffected.
            assert_eq!(r.labels("abi_delay_done"), vec![(12, 12)], "{case}");
        }
    }
}

#[test]
fn legacy_pattern_in_reverse_order_yields_one_task() {
    let r = run(costar_test_legacy_reverse_boot, 100, 1_000);
    r.assert_no_fatal();
    assert_eq!(r.labels("legacy_ran"), vec![(1, 1)]);
}

#[test]
fn timer_interrupt_wakes_blocked_task_on_time() {
    let r = run_with(
        || {
            // Virtual timer 0: periodic, every 7 ticks, on IRQ 5.
            sim_devices::timer_insert(sim_devices::VirtualTimer::new_periodic(0, 5, 7));
        },
        costar_test_timer_isr_boot,
        22,
        10_000,
    );
    r.assert_no_fatal();
    let upto_21 = |label| -> Vec<_> {
        r.labels(label)
            .into_iter()
            .filter(|&(at, _)| at <= 21)
            .collect()
    };
    assert_eq!(upto_21("timer_isr"), vec![(7, 1), (14, 1), (21, 1)]);
    assert_eq!(upto_21("isr_woke_task"), vec![(7, 1), (14, 2), (21, 3)]);
}

#[test]
fn interrupt_is_masked_in_critical_section_and_preempts_at_unmask() {
    let r = run(costar_test_isr_preemption_boot, 100, 1_000);
    r.assert_no_fatal();
    let order: Vec<_> = r
        .records
        .iter()
        .map(|&(_, label, _)| label)
        .filter(|l| {
            l.starts_with("raised")
                || *l == "soft_isr"
                || *l == "high_ran"
                || *l == "low_after_unmask"
        })
        .collect();
    assert_eq!(
        order,
        vec![
            "raised_while_masked",
            "soft_isr",
            "high_ran",
            "low_after_unmask"
        ]
    );
}

#[test]
fn external_irq_is_taken_at_the_world_instant_of_its_step() {
    let mut sim = Simulator::new(SimConfig::default());
    sim.enable_owned_devices();
    let global = sim.sim_global.clone();
    let _active = sim.activate();
    unsafe { costar_test_external_irq_boot() };
    sim.set_scheduler_limit(Some(0));
    unsafe { sim_ffi::sim_scheduler_tick() };

    // The World reaches tick 5 and stages an IRQ before stepping the idle
    // machine: the ISR and the task it wakes run at 5, not at 0.
    sim_devices::irq::with_irq_mut(|c| c.raise(6));
    sim.set_scheduler_limit(Some(5));
    unsafe { sim_ffi::sim_scheduler_tick() };

    let global = global.borrow();
    let at = |wanted: &str| -> Vec<u64> {
        global
            .trace
            .as_ref()
            .unwrap()
            .events
            .iter()
            .filter_map(|e| match e {
                TraceEvent::UserU32 { at, label, .. } if *label == wanted => Some(*at),
                _ => None,
            })
            .collect()
    };
    assert_eq!(at("timer_isr"), vec![5]);
    assert_eq!(at("isr_woke_task"), vec![5]);
    assert_eq!(global.scheduler_sim_time, 5);
}

/// Periodic timer 0 on IRQ 5 (every 7 ticks), then input on IRQ 5 staged
/// for the step to tick 20.  Returns the `UserU32` records by label.
fn run_timer_with_step_input(boot: unsafe extern "C" fn()) -> Vec<(&'static str, u64, u32)> {
    let mut sim = Simulator::new(SimConfig::default());
    sim.enable_owned_devices();
    let global = sim.sim_global.clone();
    let _active = sim.activate();
    sim_devices::timer_insert(sim_devices::VirtualTimer::new_periodic(0, 5, 7));
    unsafe { boot() };
    sim.set_scheduler_limit(Some(0));
    unsafe { sim_ffi::sim_scheduler_tick() };

    sim_devices::irq::with_irq_mut(|c| c.raise(5));
    sim.set_scheduler_limit(Some(20));
    unsafe { sim_ffi::sim_scheduler_tick() };

    let global = global.borrow();
    global
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 { at, label, value } => Some((*label, *at, *value)),
            _ => None,
        })
        .collect()
}

fn times_of(records: &[(&str, u64, u32)], label: &str) -> Vec<u64> {
    records
        .iter()
        .filter(|&&(l, _, _)| l == label)
        .map(|&(_, at, _)| at)
        .collect()
}

#[test]
fn step_input_does_not_delay_earlier_interrupts_on_its_line() {
    // The input arrives at the World instant 20; the expiries at 7 and 14
    // are still taken on time.
    let records = run_timer_with_step_input(costar_test_timer_isr_boot);
    assert_eq!(times_of(&records, "timer_isr"), vec![7, 14, 20]);
}

#[test]
fn acknowledging_an_irq_keeps_step_input_on_its_line() {
    // The ISR acknowledges each expiry with sim_irq_clear(5); that must not
    // cancel the input that arrives at 20, nor report it before then.
    let records = run_timer_with_step_input(costar_test_timer_isr_ack_boot);
    assert_eq!(times_of(&records, "timer_isr"), vec![7, 14, 20]);
    let pending: Vec<_> = records
        .iter()
        .filter(|&&(l, _, _)| l == "pending_after_ack")
        .map(|&(_, at, v)| (at, v))
        .collect();
    assert_eq!(pending, vec![(7, u32::MAX), (14, u32::MAX), (20, u32::MAX)]);
}

#[test]
fn budget_exhausted_in_isr_does_not_switch_tasks_mid_isr() {
    let r = run(costar_test_isr_budget_boot, 10, 100);
    r.assert_no_fatal();
    let order: Vec<_> = r
        .records
        .iter()
        .map(|&(_, label, _)| label)
        .filter(|l| ["isr_start", "isr_end", "high_ran", "low_after_isr"].contains(l))
        .collect();
    assert_eq!(
        order,
        vec!["isr_start", "isr_end", "high_ran", "low_after_isr"]
    );
}
