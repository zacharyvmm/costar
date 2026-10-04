//! A thread that exits with a standalone task that never ran.
//!
//! The task still owns its captures.  At thread exit the standalone
//! (thread-local) task table is being destroyed, so a capture destructor
//! that calls back into the simulator (`spawn_rust_task()`) could not reach
//! it: dropping the task there aborted the process.  The table leaks such
//! tasks at thread exit instead, and a simulator call that finds the state
//! gone reports it (task id 0) rather than panicking.  A `Simulator`'s own
//! table still drops the captures.

use std::sync::atomic::{AtomicU32, Ordering};

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;

static DROPS: AtomicU32 = AtomicU32::new(0);

struct Capture;

impl Drop for Capture {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::SeqCst);
        sim_ffi::spawn_rust_task("from_drop", 1, 65536, |_| {});
    }
}

#[test]
fn a_reentrant_capture_of_a_never_run_task_at_thread_exit() {
    std::thread::spawn(|| {
        let capture = Capture;
        let id = sim_ffi::spawn_rust_task("never_started", 1, 65536, move |_| drop(capture));
        assert_ne!(id, 0);
    })
    .join()
    .unwrap();
}

#[test]
fn an_owned_simulator_still_drops_the_captures() {
    let before = DROPS.load(Ordering::SeqCst);
    let mut sim = Simulator::new(SimConfig::default());
    {
        let _active = sim.activate();
        let capture = Capture;
        sim_ffi::spawn_rust_task("never_started", 1, 65536, move |_| drop(capture));
    }
    drop(sim);
    assert!(DROPS.load(Ordering::SeqCst) > before);
}
