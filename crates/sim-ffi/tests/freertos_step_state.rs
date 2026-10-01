//! Stepping-state regressions: a task created after the machine went
//! quiescent, and an owed budget tick when the caller switches from
//! bounded (World) to unbounded stepping.

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;
extern "C" {
    fn costar_test_abi_delay_boot();
}
#[test]
fn spawned_native_task_clears_quiescence() {
    let mut sim = Simulator::new(SimConfig::default());
    {
        let _active = sim.activate();
        unsafe {
            costar_test_abi_delay_boot();
        }
        sim_ffi::with_global_mut(|g| g.scheduler_limit = Some(12));
        assert_eq!(unsafe { sim_ffi::sim_scheduler_tick() }, 0);
    }
    assert!(!sim.has_runnable_fiber());
    {
        let _active = sim.activate();
        sim_ffi::spawn_rust_task("new", 5, 4096, |_| {});
    }
    assert!(
        sim.has_runnable_fiber(),
        "new native task is ready but simulator reports no runnable fiber"
    );
}
