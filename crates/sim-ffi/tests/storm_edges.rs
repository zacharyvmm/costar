//! Edges of the interrupt-storm rule.
//!
//! - Work held off by the interrupt mask is no storm, even at the delivery
//!   cap: an ISR that masks interrupts on the last delivery the cap allows
//!   leaves the rest pending for the unmask.
//! - A peripheral callback in flight when its IRQ storms the machine (host
//!   device code, not a task) runs to its end, but nothing it requests
//!   takes effect: no further ISR, IRQ, callback or trace, and the
//!   dispatcher runs no further callback.
//! - An ISR that stops its machine (here through a callback storm it sets
//!   off) ends the delivery batch: no further ISR of the batch runs.

use std::cell::Cell;

use sim_core::{SimConfig, TraceEvent};
use sim_ffi::device_ffi::{sim_irq_raise, sim_irq_set_handler};
use sim_ffi::freertos::{sim_disable_interrupts, sim_enable_interrupts};
use sim_ffi::simulator::Simulator;

thread_local! {
    static LIMIT: Cell<u32> = const { Cell::new(0) };
    static ISR_RUNS: Cell<u32> = const { Cell::new(0) };
    static ISR_RUNS_STOPPED: Cell<u32> = const { Cell::new(0) };
    static ORDER: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
    static CALLBACK_LOOPS: Cell<u32> = const { Cell::new(0) };
    static LATE_CALLBACKS: Cell<u32> = const { Cell::new(0) };
    static FLAG: Cell<bool> = const { Cell::new(false) };
}

fn count_isr() -> u32 {
    if sim_ffi::freertos::fatally_stopped() {
        ISR_RUNS_STOPPED.with(|c| c.set(c.get() + 1));
    }
    ISR_RUNS.with(|c| {
        c.set(c.get() + 1);
        c.get()
    })
}

fn storms(sim: &Simulator) -> usize {
    sim_ffi::flush_trace();
    sim.sim_global
        .borrow()
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                TraceEvent::UserU32 {
                    label: "irq_storm",
                    ..
                }
            )
        })
        .count()
}

/// Retriggers IRQ 7 until the cap's last delivery, which retriggers it once
/// more and masks interrupts.
unsafe extern "C" fn retrigger_then_mask() {
    let n = count_isr();
    let limit = LIMIT.with(Cell::get);
    if n <= limit {
        sim_irq_raise(7);
    }
    if n == limit {
        sim_disable_interrupts();
    }
}

#[test]
fn masking_on_the_last_delivery_of_the_cap_is_no_storm() {
    for limit in [1024, 1] {
        LIMIT.with(|l| l.set(limit));
        ISR_RUNS.with(|c| c.set(0));
        let mut sim = Simulator::new(SimConfig::default());
        sim.enable_owned_devices();
        sim.set_storm_limit(limit);
        let _a = sim.activate();
        unsafe {
            sim_irq_set_handler(7, Some(retrigger_then_mask));
            sim_irq_raise(7);
        }
        // `limit` ISRs ran, the last masked interrupts with IRQ 7 pending.
        assert_eq!(ISR_RUNS.with(Cell::get), limit, "limit={limit}");
        assert!(!sim_ffi::freertos::halted(), "limit={limit}: false storm");
        assert_eq!(storms(&sim), 0, "limit={limit}");
        // The unmask takes the IRQ left pending.
        sim_enable_interrupts();
        assert_eq!(ISR_RUNS.with(Cell::get), limit + 1, "limit={limit}");
        assert!(!sim_ffi::freertos::halted(), "limit={limit}");
    }
}

unsafe extern "C" fn record_and_mask() {
    count_isr();
    ORDER.with(|o| o.borrow_mut().push(7));
    sim_disable_interrupts();
}

unsafe extern "C" fn record_8() {
    count_isr();
    ORDER.with(|o| o.borrow_mut().push(8));
}

#[test]
fn an_isr_masking_with_another_irq_pending_at_limit_one_is_no_storm() {
    ORDER.with(|o| o.borrow_mut().clear());
    let mut sim = Simulator::new(SimConfig::default());
    sim.enable_owned_devices();
    sim.set_storm_limit(1);
    let _a = sim.activate();
    unsafe {
        sim_irq_set_handler(7, Some(record_and_mask));
        sim_irq_set_handler(8, Some(record_8));
        sim_disable_interrupts();
        sim_irq_raise(7);
        sim_irq_raise(8);
    }
    // IRQ 7's ISR masks interrupts; IRQ 8 waits.
    sim_enable_interrupts();
    assert_eq!(ORDER.with(|o| o.borrow().clone()), vec![7]);
    assert!(!sim_ffi::freertos::halted(), "false storm");
    sim_enable_interrupts();
    assert_eq!(ORDER.with(|o| o.borrow().clone()), vec![7, 8]);
    assert!(!sim_ffi::freertos::halted());
    assert_eq!(storms(&sim), 0);
}

/// IRQ 9's ISR re-raises it for ever: a storm.
unsafe extern "C" fn storming_isr() {
    count_isr();
    sim_irq_raise(9);
}

/// Raises IRQ 9 until an ISR sets the flag — which never happens: the
/// storm stops the machine first, and every later raise is a no-op.  The
/// loop is bounded so a broken stop shows as a failure, not a hang.
unsafe extern "C" fn raise_until_flag() {
    while !FLAG.with(Cell::get) && CALLBACK_LOOPS.with(Cell::get) < 10_000 {
        CALLBACK_LOOPS.with(|c| c.set(c.get() + 1));
        sim_irq_raise(9);
        sim_ffi::sim_schedule_event(sim_ffi::sim_now_ticks() + 1, Some(late_callback));
        sim_ffi::sim_trace_u32(c"after_stop".as_ptr(), 1);
    }
}

unsafe extern "C" fn late_callback() {
    LATE_CALLBACKS.with(|c| c.set(c.get() + 1));
}

#[test]
fn a_callback_in_flight_finishes_but_nothing_it_requests_takes_effect() {
    for freertos in [false, true] {
        let case = format!("freertos={freertos}");
        ISR_RUNS.with(|c| c.set(0));
        ISR_RUNS_STOPPED.with(|c| c.set(0));
        CALLBACK_LOOPS.with(|c| c.set(0));
        LATE_CALLBACKS.with(|c| c.set(0));
        let mut sim = Simulator::new(SimConfig::default());
        sim.enable_owned_devices();
        sim.set_storm_limit(16);
        let g = sim.sim_global.clone();
        let _a = sim.activate();
        unsafe {
            sim_irq_set_handler(9, Some(storming_isr));
            sim_ffi::sim_schedule_event(1, Some(raise_until_flag));
            // Due with the storming callback: never dispatched once it
            // stopped the machine.
            sim_ffi::sim_schedule_event(1, Some(late_callback));
            sim_ffi::sim_schedule_event(2, Some(late_callback));
        }
        if freertos {
            extern "C" {
                fn costar_test_spawn_task(
                    name: *const std::ffi::c_char,
                    body: extern "C" fn(),
                    priority: u32,
                );
            }
            extern "C" fn sleeper() {
                loop {
                    unsafe { sim_ffi::sim_task_delay_until(sim_ffi::sim_now_ticks() + 5) };
                }
            }
            unsafe { costar_test_spawn_task(c"sleeper".as_ptr(), sleeper, 1) };
        }
        let mut steps = 0;
        while unsafe { sim_ffi::sim_scheduler_tick() } != 0 {
            steps += 1;
            assert!(steps < 1_000, "{case}: never stopped");
        }
        assert!(sim_ffi::freertos::fatally_stopped(), "{case}");
        // The storm: `limit` ISRs, then no ISR again.
        assert_eq!(ISR_RUNS.with(Cell::get), 16, "{case}");
        assert_eq!(ISR_RUNS_STOPPED.with(Cell::get), 0, "{case}");
        // The callback in flight ran to its end ...
        assert_eq!(CALLBACK_LOOPS.with(Cell::get), 10_000, "{case}");
        // ... but its callbacks and traces never took effect, and no
        // other callback was dispatched.
        assert_eq!(LATE_CALLBACKS.with(Cell::get), 0, "{case}");
        assert!(
            sim_ffi::next_event_deadline().is_some_and(|t| t <= 2),
            "{case}"
        );
        sim_ffi::flush_trace();
        let g = g.borrow();
        let events = &g.trace.as_ref().unwrap().events;
        assert!(
            !events.iter().any(|e| matches!(
                e,
                TraceEvent::UserU32 {
                    label: "after_stop",
                    ..
                }
            )),
            "{case}: a trace after the stop took effect"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, TraceEvent::Fatal { .. }))
                .count(),
            1,
            "{case}"
        );
    }
}

unsafe extern "C" fn reschedule_now() {
    sim_ffi::sim_schedule_event(sim_ffi::sim_now_ticks(), Some(reschedule_now));
}

/// IRQ 6: sets off a callback storm that stops the machine.
unsafe extern "C" fn first_stops_the_machine() {
    ORDER.with(|o| o.borrow_mut().push(6));
    sim_ffi::sim_schedule_event(sim_ffi::sim_now_ticks(), Some(reschedule_now));
    sim_ffi::dispatch_events(sim_ffi::sim_now_ticks());
    assert!(sim_ffi::freertos::halted());
}

unsafe extern "C" fn second() {
    ORDER.with(|o| o.borrow_mut().push(7));
}

#[test]
fn an_isr_that_stops_the_machine_ends_its_delivery_batch() {
    for freertos in [false, true] {
        ORDER.with(|o| o.borrow_mut().clear());
        let mut sim = Simulator::new(SimConfig::default());
        sim.enable_owned_devices();
        sim.set_storm_limit(8);
        let _a = sim.activate();
        if freertos {
            extern "C" {
                fn costar_test_spawn_task(
                    name: *const std::ffi::c_char,
                    body: extern "C" fn(),
                    priority: u32,
                );
            }
            extern "C" fn sleeper() {
                loop {
                    unsafe { sim_ffi::sim_task_delay_until(sim_ffi::sim_now_ticks() + 5) };
                }
            }
            unsafe { costar_test_spawn_task(c"sleeper".as_ptr(), sleeper, 1) };
        }
        unsafe {
            sim_irq_set_handler(6, Some(first_stops_the_machine));
            sim_irq_set_handler(7, Some(second));
        }
        // Both due in scheduler context at tick 2.
        sim_devices::irq::with_irq_mut(|c| {
            c.raise_at(6, 2);
            c.raise_at(7, 2);
        });
        let mut steps = 0;
        while unsafe { sim_ffi::sim_scheduler_tick() } != 0 {
            steps += 1;
            assert!(steps < 1_000, "freertos={freertos}: never stopped");
        }
        assert!(sim_ffi::freertos::halted(), "freertos={freertos}");
        assert_eq!(
            ORDER.with(|o| o.borrow().clone()),
            vec![6],
            "freertos={freertos}: the batch went on after its machine stopped"
        );
        assert_eq!(storms(&sim), 1, "freertos={freertos}");
    }
}
