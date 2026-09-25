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
    #[cfg(unix)]
    fn costar_test_io_wait_boot(recv_fd: i32, send_fd: i32);
    #[cfg(unix)]
    fn costar_test_io_delete_boot(fd: i32, reuse: i32);
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
    let mut sim = Simulator::new(SimConfig::default());
    let global = sim.sim_global.clone();
    {
        let _active = sim.activate();
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
