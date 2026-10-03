//! Switching between bounded (World) and unbounded stepping keeps the
//! scheduling decisions FreeRTOS made: with configUSE_TIME_SLICING 0 an
//! equal-priority busy task is never rotated out by a mode change.

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;
use std::sync::{Arc, Mutex};
extern "C" {
    fn costar_test_abi_delay_boot();
}
fn run_case(park: bool, unbounded_first: bool) -> Vec<(&'static str, u64)> {
    let mut sim = Simulator::new(SimConfig::default());
    let g = sim.sim_global.clone();
    let _a = sim.activate();
    unsafe { costar_test_abi_delay_boot() };
    if park {
        g.borrow_mut().scheduler_limit = Some(12);
        assert_eq!(unsafe { sim_ffi::sim_scheduler_tick() }, 0);
    } else {
        for _ in 0..100 {
            if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
                break;
            }
        }
    }
    assert_eq!(g.borrow().scheduler_sim_time, 12);
    let records = Arc::new(Mutex::new(Vec::new()));
    for name in ["a", "b"] {
        let records = records.clone();
        sim_ffi::spawn_rust_task(name, 5, 65536, move |ctx| unsafe {
            sim_ffi::sim_budget_set_limit(1);
            while ctx.now() < 16 {
                records.lock().unwrap().push((name, ctx.now()));
                sim_ffi::sim_budget_poll(std::ptr::null(), 0);
            }
        });
    }
    g.borrow_mut().scheduler_limit = if unbounded_first { None } else { Some(12) };
    unsafe { sim_ffi::sim_scheduler_tick() };
    g.borrow_mut().scheduler_limit = Some(14);
    unsafe { sim_ffi::sim_scheduler_tick() };
    let out = records.lock().unwrap().clone();
    out
}
#[test]
fn parked_state_survives_unbounded() {
    let expected = vec![("a", 12), ("a", 13), ("a", 14)];
    assert_eq!(run_case(false, true), expected, "unparked control");
    assert_eq!(run_case(true, false), expected, "bounded control");
    assert_eq!(
        run_case(true, true),
        expected,
        "bounded -> unbounded -> bounded"
    );
}
