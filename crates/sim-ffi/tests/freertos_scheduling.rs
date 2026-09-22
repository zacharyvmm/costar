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
    fn costar_test_timer_isr_boot();
    fn costar_test_isr_preemption_boot();
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
