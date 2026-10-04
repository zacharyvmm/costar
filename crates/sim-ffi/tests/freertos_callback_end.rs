//! A peripheral callback that ends the scheduler (`vTaskEndScheduler()`)
//! stops the machine on every scheduler path: the step makes no kernel
//! call after it (no switch, no tick), reports completion and asks for no
//! wake-up — idle, with a budget tick owed, masked or not, bounded or not.

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;

extern "C" {
    fn costar_test_abi_delay_boot();
    fn vTaskEndScheduler();
}

unsafe extern "C" fn end_callback() {
    vTaskEndScheduler();
}

/// The machine is stopped for good: completion, no wake, later steps too.
fn assert_ended(sim: &Simulator, more: u32, case: &str) {
    assert_eq!(
        (more, sim.freertos_next_wake()),
        (0, None),
        "{case}: the step did not report the end"
    );
    assert!(sim.sim_global.borrow().freertos_ended, "{case}");
    for _ in 0..3 {
        assert_eq!(unsafe { sim_ffi::sim_scheduler_tick() }, 0, "{case}");
    }
}

/// An idle machine, a callback at tick 1 that ends the scheduler, a step
/// bounded at 2 (or unbounded).
#[test]
fn a_callback_ending_the_scheduler_of_an_idle_machine() {
    for masked in [false, true] {
        for bounded in [true, false] {
            let case = format!("masked={masked} bounded={bounded}");
            let mut sim = Simulator::new(SimConfig::default());
            let _active = sim.activate();
            unsafe { costar_test_abi_delay_boot() };
            sim.set_scheduler_limit(Some(0));
            unsafe { sim_ffi::sim_scheduler_tick() };
            unsafe { sim_ffi::sim_schedule_event(1, Some(end_callback)) };
            if masked {
                sim_ffi::freertos::sim_disable_interrupts();
            }
            sim.set_scheduler_limit(bounded.then_some(2));
            let mut more = 1;
            for _ in 0..10 {
                more = unsafe { sim_ffi::sim_scheduler_tick() };
                if more == 0 {
                    break;
                }
            }
            assert_ended(&sim, more, &case);
        }
    }
}

/// A busy task owes a budget tick at bounded tick 0; a callback due at
/// tick 0 ends the scheduler in the next step within the tick (or in the
/// next unbounded step, which charges the owed tick first).
#[test]
fn a_callback_ending_the_scheduler_while_a_tick_is_owed() {
    for masked in [false, true] {
        for bounded in [true, false] {
            let case = format!("masked={masked} bounded={bounded}");
            let mut sim = Simulator::new(SimConfig::default());
            let _active = sim.activate();
            unsafe { costar_test_abi_delay_boot() };
            sim.set_scheduler_limit(Some(0));
            unsafe { sim_ffi::sim_scheduler_tick() };
            sim_ffi::spawn_rust_task("busy", 7, 65536, move |_| unsafe {
                if masked {
                    sim_ffi::sim_enter_critical();
                }
                sim_ffi::sim_budget_set_limit(1);
                loop {
                    sim_ffi::sim_budget_poll(std::ptr::null(), 0);
                }
            });
            unsafe { sim_ffi::sim_scheduler_tick() };
            assert_eq!(sim.freertos_next_wake(), Some(1), "{case}: no owed tick");
            unsafe { sim_ffi::sim_schedule_event(0, Some(end_callback)) };
            sim.set_scheduler_limit(bounded.then_some(0));
            let more = unsafe { sim_ffi::sim_scheduler_tick() };
            if !bounded {
                // Unbounded steps report no wake-up of their own.
                sim.set_scheduler_limit(Some(0));
            }
            assert_ended(&sim, more, &case);
        }
    }
}

/// Ending one machine's scheduler outside a task (here from host code)
/// deletes its idle and timer tasks; those deletions are applied to that
/// machine at once.  A new machine on the same thread then runs normally:
/// it used to receive the ended machine's deletions and crash when its own
/// task reused a freed TCB's address.
#[test]
fn a_machine_after_one_whose_scheduler_ended_runs_normally() {
    for _ in 0..3 {
        let mut sim = Simulator::new(SimConfig::default());
        let _active = sim.activate();
        unsafe { costar_test_abi_delay_boot() };
        sim.set_scheduler_limit(Some(0));
        unsafe { sim_ffi::sim_scheduler_tick() };
        unsafe { vTaskEndScheduler() };
        assert_ended(&sim, 0, "host end");
    }
}
