//! Ending the scheduler between steps (host code calling
//! `vTaskEndScheduler()`) clears the machine's next wake-up.

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;
extern "C" {
    fn costar_test_abi_delay_boot();
    fn vTaskEndScheduler();
}
#[test]
fn ending_between_steps_clears_next_wake() {
    let mut sim = Simulator::new(SimConfig::default());
    let g = sim.sim_global.clone();
    let _a = sim.activate();
    unsafe { costar_test_abi_delay_boot() };
    g.borrow_mut().scheduler_limit = Some(0);
    assert_eq!(unsafe { sim_ffi::sim_scheduler_tick() }, 1);
    assert!(sim.freertos_next_wake().is_some());
    unsafe { vTaskEndScheduler() };
    assert_eq!(unsafe { sim_ffi::sim_scheduler_tick() }, 0);
    assert_eq!(sim.freertos_next_wake(), None);
}
