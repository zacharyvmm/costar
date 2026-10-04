//! `sim_start_scheduler()` is the same scheduler as `sim_scheduler_tick()`:
//! it starts FreeRTOS (idle and timer tasks) before running its tasks.
//! (See also `freertos_standalone_start.rs`.)

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;

extern "C" {
    fn costar_test_abi_delay_boot();
}

#[test]
fn start_wrapper_starts_the_kernel_like_stepping_does() {
    let mut sim = Simulator::new(SimConfig::default());
    let g = sim.sim_global.clone();
    {
        let _a = sim.activate();
        unsafe {
            costar_test_abi_delay_boot();
            // Used to run without an idle task and crash once tasks blocked.
            sim_ffi::sim_start_scheduler();
        }
    }
    let g = g.borrow();
    let done: Vec<_> = g
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match e {
            sim_core::trace::TraceEvent::UserU32 {
                at,
                label: "abi_delay_done",
                value,
            } => Some((*at, *value)),
            _ => None,
        })
        .collect();
    assert_eq!(done, vec![(12, 12)]);
}
