//! Edges of the interrupt-storm rule.
//!
//! - Work held off by the interrupt mask is no storm, even at the delivery
//!   cap: an ISR that masks interrupts on the last delivery the cap allows
//!   leaves the rest pending for the unmask.

use std::cell::Cell;

use sim_core::{SimConfig, TraceEvent};
use sim_ffi::device_ffi::{sim_irq_raise, sim_irq_set_handler};
use sim_ffi::freertos::{sim_disable_interrupts, sim_enable_interrupts};
use sim_ffi::simulator::Simulator;

thread_local! {
    static LIMIT: Cell<u32> = const { Cell::new(0) };
    static ISR_RUNS: Cell<u32> = const { Cell::new(0) };
    static ORDER: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn count_isr() -> u32 {
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
