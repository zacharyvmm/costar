//! FreeRTOS machine lifecycle regressions: budget ticks across World steps
//! within one tick, legacy task matching after a task has gone, stepping
//! after `vTaskEndScheduler()`, and the longest finite delay.
//!
//! Each test drives a `Simulator` directly through the C ABI.

use sim_core::{trace::TraceEvent, SimConfig};
use sim_ffi::simulator::Simulator;
use std::ffi::{c_char, c_long, c_ulong, c_void};

extern "C" {
    fn costar_test_abi_delay_boot();
    fn xTaskCreate(
        entry: unsafe extern "C" fn(*mut c_void),
        name: *const c_char,
        depth: u16,
        arg: *mut c_void,
        priority: c_ulong,
        handle: *mut *mut c_void,
    ) -> c_long;
    fn vTaskDelay(ticks: u32);
    fn vTaskEndScheduler();
}

fn records(
    global: &std::rc::Rc<std::cell::RefCell<sim_ffi::SimGlobal>>,
    label: &str,
) -> Vec<(u64, u32)> {
    global
        .borrow()
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 {
                at,
                label: l,
                value,
            } if *l == label => Some((*at, *value)),
            _ => None,
        })
        .collect()
}

#[test]
fn same_tick_step_must_not_resume_a_budget_exhausted_task() {
    let mut sim = Simulator::new(SimConfig::default());
    let global = sim.sim_global.clone();
    let _active = sim.activate();
    unsafe {
        costar_test_abi_delay_boot();
    }
    sim_ffi::spawn_rust_task("high", 6, 4096, |ctx| {
        ctx.sleep_for(1);
        unsafe {
            sim_ffi::sim_trace_u32(c"high_woke".as_ptr(), ctx.now() as u32);
        }
    });
    sim_ffi::spawn_rust_task("low", 5, 4096, |ctx| unsafe {
        sim_ffi::sim_budget_set_limit(1);
        sim_ffi::sim_budget_poll(std::ptr::null(), 1);
        sim_ffi::sim_trace_u32(c"after_budget".as_ptr(), ctx.now() as u32);
    });
    global.borrow_mut().scheduler_limit = Some(0);
    unsafe {
        sim_ffi::sim_scheduler_tick();
    }
    assert!(records(&global, "after_budget").is_empty());
    // World advances 500 us, which still maps to tick limit 0.
    unsafe {
        sim_ffi::sim_scheduler_tick();
    }
    assert!(
        records(&global, "after_budget").is_empty(),
        "resumed before tick 1: {:?}",
        records(&global, "after_budget")
    );
}
