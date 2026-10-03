//! A FreeRTOS-adopted native task that masks interrupts and polls its
//! budget: the masked-budget check calls into C (the kernel's current-task
//! accessors) and must not hold the budget state borrowed meanwhile, or an
//! instrumented build — where those accessors themselves call
//! `sim_budget_poll()` — re-enters it and aborts ("RefCell already
//! borrowed").
//!
//! Meaningful with edge instrumentation: `tests/golden_trace_test.sh`
//! runs it with `SIM_INSTRUMENT_EDGES=1` next to the tight-loop golden.
//! The task polls for 5000 ticks so the edge hook's throttle (every 10 000
//! edges) fires inside the accessors.  Without instrumentation it still
//! checks that the masked task's CPU time is charged.

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;
use std::sync::{Arc, Mutex};

extern "C" {
    fn costar_test_abi_delay_boot();
}

#[test]
fn a_masked_adopted_task_polling_its_budget_under_instrumentation() {
    let mut sim = Simulator::new(SimConfig::default());
    let _active = sim.activate();
    unsafe { costar_test_abi_delay_boot() };
    let reached = Arc::new(Mutex::new(None));
    let result = reached.clone();
    sim_ffi::spawn_rust_task("masked", 7, 65536, move |ctx| unsafe {
        sim_ffi::sim_enter_critical();
        sim_ffi::sim_budget_set_limit(1);
        sim_ffi::sim_budget_reset();
        while ctx.now() < 5_000 {
            sim_ffi::sim_budget_poll(std::ptr::null(), 0);
        }
        *result.lock().unwrap() = Some(ctx.now());
        sim_ffi::sim_budget_set_limit(1_000_000);
        sim_ffi::sim_exit_critical();
    });
    sim.set_scheduler_limit(Some(6_000));
    for _ in 0..10 {
        unsafe { sim_ffi::sim_scheduler_tick() };
    }
    assert_eq!(*reached.lock().unwrap(), Some(5_000));
}
