//! Stepping-state regressions: a task created after the machine went
//! quiescent, and an owed budget tick when the caller switches from
//! bounded (World) to unbounded stepping.

use sim_core::{trace::TraceEvent, SimConfig};
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

#[test]
fn changing_to_unbounded_stepping_charges_owed_tick() {
    let mut sim = Simulator::new(SimConfig::default());
    let g = sim.sim_global.clone();
    {
        let _active = sim.activate();
        unsafe {
            costar_test_abi_delay_boot();
        }
        sim_ffi::spawn_rust_task("low", 5, 4096, |ctx| unsafe {
            sim_ffi::sim_budget_set_limit(1);
            sim_ffi::sim_budget_poll(std::ptr::null(), 0);
            sim_ffi::sim_trace_u32(c"resumed".as_ptr(), ctx.now() as u32);
        });
        g.borrow_mut().scheduler_limit = Some(0);
        unsafe {
            sim_ffi::sim_scheduler_tick();
        }
        g.borrow_mut().scheduler_limit = None;
        unsafe {
            sim_ffi::sim_scheduler_tick();
        }
    }
    let records: Vec<_> = g
        .borrow()
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 {
                at,
                label: "resumed",
                value,
            } => Some((*at, *value)),
            _ => None,
        })
        .collect();
    assert_eq!(records, vec![(1, 1)]);
}
