//! Resuming the handle of a task that faulted does not end the machine:
//! FreeRTOS may select the dead task, which is suspended again, and the
//! machine's live tasks keep running.

use sim_core::{SimConfig, TraceEvent};
use sim_ffi::simulator::Simulator;
use std::ffi::c_void;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
extern "C" {
    fn costar_test_abi_delay_boot();
    fn xTaskGetCurrentTaskHandle() -> *mut c_void;
    fn vTaskResume(t: *mut c_void);
}
#[test]
fn resuming_a_faulted_task_does_not_end_the_other_tasks() {
    for world in [true, false] {
        fault_resume_case(world);
    }
}

fn fault_resume_case(world: bool) {
    let mut sim = Simulator::new(SimConfig::default());
    let g = sim.sim_global.clone();
    let _a = sim.activate();
    unsafe { costar_test_abi_delay_boot() };
    let h = Arc::new(AtomicUsize::new(0));
    let save = h.clone();
    sim_ffi::spawn_rust_task("fault", 6, 65536, move |_| {
        save.store(
            unsafe { xTaskGetCurrentTaskHandle() } as usize,
            Ordering::SeqCst,
        );
        panic!("task fault");
    });
    sim_ffi::spawn_rust_task("supervisor", 5, 65536, move |ctx| {
        ctx.sleep_for(1);
        unsafe {
            vTaskResume(h.load(Ordering::SeqCst) as *mut c_void);
            sim_ffi::sim_trace_u32(c"supervisor_continues".as_ptr(), 1);
        }
    });
    if world {
        g.borrow_mut().scheduler_limit = Some(20);
        unsafe { sim_ffi::sim_scheduler_tick() };
    } else {
        for _ in 0..1_000 {
            if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
                break;
            }
        }
    }
    let b = g.borrow();
    let labels: Vec<_> = b
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter_map(|e| {
            if let TraceEvent::UserU32 { label, .. } = e {
                Some(*label)
            } else {
                None
            }
        })
        .collect();
    assert!(
        labels.contains(&"supervisor_continues"),
        "a ready supervisor vanished"
    );
    assert!(
        labels.contains(&"abi_delay_done"),
        "healthy firmware task vanished"
    );
}
