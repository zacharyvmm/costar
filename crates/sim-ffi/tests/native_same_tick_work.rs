//! Work created for the current tick runs at that tick, on the native
//! scheduler (also behind the Zephyr scheduler step) and the Zephyr
//! scheduler loop: an ISR's callback for now, a timer re-armed with zero
//! delay.  Neither time moves nor does the scheduler report completion
//! while work is due at the current tick; repeating it without end is an
//! interrupt storm, bounded by the machine's storm limit.

use std::cell::{Cell, RefCell};

use sim_core::{SimConfig, Tick};
use sim_ffi::device_ffi::{sim_irq_set_handler, sim_timer_arm};
use sim_ffi::simulator::Simulator;

const IRQ: u32 = 4;
const TIMER_IRQ: u32 = 6;

thread_local! {
    static CALLBACK_TICKS: RefCell<Vec<Tick>> = const { RefCell::new(Vec::new()) };
    static TIMER_TICKS: RefCell<Vec<Tick>> = const { RefCell::new(Vec::new()) };
    /// How often the timer ISR re-arms its timer with zero delay.
    static REARMS: Cell<u32> = const { Cell::new(0) };
}

#[derive(Clone, Copy, Debug)]
enum Driver {
    /// `sim_start_scheduler()`: native stepping to completion.
    Start,
    /// `sim_scheduler_tick()` under a World-style limit.
    Bounded,
    /// `sim_zephyr_scheduler_tick()` (the native step).
    ZephyrStep,
    /// `sim_zephyr_start_scheduler()` (the Zephyr loop).
    ZephyrLoop,
}

const DRIVERS: [Driver; 4] = [
    Driver::Start,
    Driver::Bounded,
    Driver::ZephyrStep,
    Driver::ZephyrLoop,
];

fn run(driver: Driver, sim: &mut Simulator) {
    match driver {
        Driver::Start => unsafe { sim_ffi::sim_start_scheduler() },
        Driver::ZephyrLoop => unsafe { sim_ffi::zephyr_ffi::sim_zephyr_start_scheduler() },
        Driver::Bounded => {
            for limit in 0..20 {
                sim.set_scheduler_limit(Some(limit));
                while unsafe { sim_ffi::sim_scheduler_tick() } != 0 {}
            }
        }
        Driver::ZephyrStep => {
            for _ in 0..1_000 {
                if unsafe { sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick() } == 0 {
                    break;
                }
            }
        }
    }
}

unsafe extern "C" fn record_callback() {
    CALLBACK_TICKS.with(|t| t.borrow_mut().push(sim_ffi::sim_now_ticks()));
}

unsafe extern "C" fn isr_schedules_now() {
    sim_ffi::sim_schedule_event(sim_ffi::sim_now_ticks(), Some(record_callback));
}

#[test]
fn an_isr_callback_for_the_current_tick_runs_at_that_tick() {
    for driver in DRIVERS {
        CALLBACK_TICKS.with(|t| t.borrow_mut().clear());
        let mut sim = Simulator::new(SimConfig::default());
        sim.enable_owned_devices();
        let _active = sim.activate();
        unsafe { sim_irq_set_handler(IRQ, Some(isr_schedules_now)) };
        // IRQ input at tick 0 to an idle machine.
        sim_devices::irq::with_irq_mut(|c| c.raise_at(IRQ, 0));
        run(driver, &mut sim);
        assert_eq!(
            CALLBACK_TICKS.with(|t| t.borrow().clone()),
            vec![0],
            "{driver:?}: the ISR's callback for now did not run at its tick"
        );
        assert!(!sim_ffi::freertos::halted(), "{driver:?}");
    }
}

unsafe extern "C" fn timer_isr() {
    TIMER_TICKS.with(|t| t.borrow_mut().push(sim_ffi::sim_now_ticks()));
    let left = REARMS.with(Cell::get);
    if left > 0 {
        REARMS.with(|r| r.set(left - 1));
        sim_timer_arm(0, 0);
    }
}

#[test]
fn a_timer_rearmed_with_zero_delay_fires_every_time_at_its_tick() {
    for driver in DRIVERS {
        TIMER_TICKS.with(|t| t.borrow_mut().clear());
        REARMS.with(|r| r.set(2));
        let mut sim = Simulator::new(SimConfig::default());
        sim.enable_owned_devices();
        let _active = sim.activate();
        unsafe { sim_irq_set_handler(TIMER_IRQ, Some(timer_isr)) };
        sim_devices::timer_insert(sim_devices::VirtualTimer::new_oneshot(0, TIMER_IRQ));
        unsafe { sim_timer_arm(0, 5) };
        run(driver, &mut sim);
        assert_eq!(
            TIMER_TICKS.with(|t| t.borrow().clone()),
            vec![5, 5, 5],
            "{driver:?}: a zero-delay re-arm was dropped"
        );
        assert!(!sim_ffi::freertos::halted(), "{driver:?}");
    }
}

unsafe extern "C" fn timer_isr_forever() {
    TIMER_TICKS.with(|t| t.borrow_mut().push(sim_ffi::sim_now_ticks()));
    sim_timer_arm(0, 0);
}

/// A timer ISR re-arming its one-shot with zero delay without end is an
/// interrupt storm on every native driver: the machine stops at the storm
/// limit, at the timer's tick.
#[test]
fn a_timer_rearming_itself_for_ever_is_a_storm() {
    for driver in DRIVERS {
        TIMER_TICKS.with(|t| t.borrow_mut().clear());
        let mut sim = Simulator::new(SimConfig::default());
        sim.enable_owned_devices();
        sim.set_storm_limit(4);
        let _active = sim.activate();
        unsafe { sim_irq_set_handler(TIMER_IRQ, Some(timer_isr_forever)) };
        sim_devices::timer_insert(sim_devices::VirtualTimer::new_oneshot(0, TIMER_IRQ));
        unsafe { sim_timer_arm(0, 5) };
        run(driver, &mut sim);
        assert!(sim_ffi::freertos::halted(), "{driver:?}: no storm");
        let ticks = TIMER_TICKS.with(|t| t.borrow().clone());
        assert!(
            !ticks.is_empty() && ticks.len() <= 6 && ticks.iter().all(|&t| t == 5),
            "{driver:?}: {ticks:?}"
        );
    }
}
