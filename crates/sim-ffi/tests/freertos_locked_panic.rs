//! A task that panics while holding the scheduler lock (`vTaskSuspendAll`)
//! is isolated like any faulted task: the lock is released on its behalf
//! and the machine's other tasks keep running.

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
extern "C" {
    fn costar_test_abi_delay_boot();
    fn vTaskSuspendAll();
}
#[test]
fn panic_while_scheduler_locked_is_isolated() {
    let mut sim = Simulator::new(SimConfig::default());
    let g = sim.sim_global.clone();
    let _a = sim.activate();
    unsafe { costar_test_abi_delay_boot() };
    sim_ffi::spawn_rust_task("panic", 6, 65536, |_| {
        unsafe { vTaskSuspendAll() };
        panic!("panic while holding scheduler lock");
    });
    let ran = Arc::new(AtomicBool::new(false));
    let set = ran.clone();
    sim_ffi::spawn_rust_task("healthy", 5, 65536, move |_| {
        set.store(true, Ordering::SeqCst);
    });
    g.borrow_mut().scheduler_limit = Some(20);
    unsafe { sim_ffi::sim_scheduler_tick() };
    assert!(ran.load(Ordering::SeqCst));
}
