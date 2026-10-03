//! Peripheral callbacks belong to the machine that scheduled them.
//!
//! Machine A schedules callbacks at ticks 1 and 2; the first ends A's
//! scheduler (`vTaskEndScheduler()`), so the second never runs.  A is
//! dropped, and machine B then runs on the same host thread: A's leftover
//! callback must not run under B (with a host-thread queue it did, and
//! ended B at tick 2).

use std::sync::atomic::{AtomicUsize, Ordering};

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;

static FIRED: AtomicUsize = AtomicUsize::new(0);

extern "C" {
    fn costar_test_abi_delay_boot();
    fn vTaskEndScheduler();
}

unsafe extern "C" fn end() {
    vTaskEndScheduler();
}

unsafe extern "C" fn stale() {
    FIRED.fetch_add(1, Ordering::SeqCst);
    vTaskEndScheduler();
}

#[test]
fn callbacks_of_an_ended_machine_never_run_in_another_machine() {
    {
        let mut a = Simulator::new(SimConfig::default());
        let _active = a.activate();
        unsafe {
            costar_test_abi_delay_boot();
            sim_ffi::sim_schedule_event(1, Some(end));
            sim_ffi::sim_schedule_event(2, Some(stale));
        }
        a.set_scheduler_limit(Some(20));
        assert_eq!(unsafe { sim_ffi::sim_scheduler_tick() }, 0);
        assert_eq!(FIRED.load(Ordering::SeqCst), 0);
    }
    let mut b = Simulator::new(SimConfig::default());
    let _active = b.activate();
    unsafe { costar_test_abi_delay_boot() };
    for limit in [2, 5, 20] {
        b.set_scheduler_limit(Some(limit));
        unsafe { sim_ffi::sim_scheduler_tick() };
    }
    assert_eq!(
        FIRED.load(Ordering::SeqCst),
        0,
        "machine A's leftover callback ran under machine B"
    );
    assert!(
        !b.sim_global.borrow().freertos_ended,
        "machine A's leftover callback ended machine B at tick {}",
        b.scheduler_sim_time()
    );
}
