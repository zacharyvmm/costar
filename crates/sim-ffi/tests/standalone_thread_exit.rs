//! A thread that exits with a standalone task that never ran.
//!
//! The task still owns its captures.  At thread exit the standalone
//! (thread-local) task table is being destroyed, so a capture destructor
//! that calls back into the simulator (`spawn_rust_task()`) could not reach
//! it: dropping the task there aborted the process.  The table leaks such
//! tasks at thread exit instead, and a simulator call that finds the state
//! gone reports it (task id 0) rather than panicking.  A `Simulator`'s own
//! table still drops the captures.
//!
//! Instrumented C may also run during thread teardown: the budget poll and
//! the edge hook's borrow check do nothing once the simulator state is
//! gone, and `sim_create_task()` fails before calling into C.
//! `tests/golden_trace_test.sh` runs this with `SIM_INSTRUMENT_EDGES=1`.

use std::ffi::c_void;
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

extern "C" {
    /// The edge hook (`sim_coverage.c`): checks borrows (debug builds) and
    /// polls the budget every 10 000 edges.
    fn __sanitizer_cov_trace_pc_guard(guard: *mut u32);
}

unsafe extern "C" fn entry(_: *mut c_void) {}

/// Runs at thread exit, possibly after the simulator state is destroyed.
struct CreatesAtExit;

impl Drop for CreatesAtExit {
    fn drop(&mut self) {
        let handle = unsafe {
            sim_ffi::sim_create_task(
                c"after_exit".as_ptr(),
                Some(entry),
                std::ptr::null_mut(),
                128,
                1,
            )
        };
        // 0 once the state is gone; a task if this destructor ran first.
        let _ = handle;
        unsafe {
            sim_ffi::sim_trace_u32(c"after_exit".as_ptr(), 1);
            let _ = sim_ffi::sim_now_ticks();
        }
    }
}

thread_local! {
    static CREATES_AT_EXIT: CreatesAtExit = const { CreatesAtExit };
}

#[test]
fn simulator_calls_from_a_thread_local_destructor_do_not_abort() {
    std::thread::spawn(|| {
        // Registered before the simulator state, so destroyed after it.
        CREATES_AT_EXIT.with(|_| {});
        assert_ne!(sim_ffi::spawn_rust_task("never_run", 1, 65536, |_| {}), 0);
        unsafe {
            // A budget poll is due at the next edge-hook poll.
            sim_ffi::sim_budget_set_limit(1);
            sim_ffi::sim_budget_reset();
            let mut guard = 0;
            for _ in 0..9_999 {
                __sanitizer_cov_trace_pc_guard(&mut guard);
            }
        }
    })
    .join()
    .unwrap();
}
