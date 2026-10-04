//! No C code runs while the engine holds its own state borrowed.
//!
//! Debug builds check it: `sim_budget_poll()` checks every piece of engine
//! state, and under edge instrumentation the edge hook checks the task
//! table at every edge (`sim_debug_check_engine_unborrowed`).  A violation
//! fails at once, naming the state, instead of a "RefCell already
//! borrowed" abort only when a budget tick happens to land in the window.

use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;

/// Run the test `name` of this binary in a child process with
/// `COSTAR_GUARD_CHILD=1`; returns (success, stderr).
fn in_child(name: &str) -> (bool, String) {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env("COSTAR_GUARD_CHILD", "1")
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn child() -> bool {
    std::env::var_os("COSTAR_GUARD_CHILD").is_some()
}

/// The child half: polls the budget (a C entry point, as the edge hook
/// calls it) while the task table is borrowed.
#[test]
fn guard_child_polls_under_a_borrow() {
    if !child() {
        return;
    }
    let mut sim = Simulator::new(SimConfig::default());
    let _active = sim.activate();
    let _held = sim.sim_global.borrow();
    unsafe { sim_ffi::sim_budget_poll(std::ptr::null(), 0) };
}

#[test]
fn a_budget_poll_under_an_engine_borrow_fails_naming_the_state() {
    if child() || !cfg!(debug_assertions) {
        return;
    }
    let (ok, stderr) = in_child("guard_child_polls_under_a_borrow");
    assert!(!ok, "the poll under a borrow went unnoticed");
    assert!(
        stderr.contains("C code ran while the engine held the simulator state"),
        "{stderr}"
    );
}

/// The Zephyr scheduler step runs a task (guest code that polls its
/// budget) without holding its tick state borrowed.
#[test]
fn the_zephyr_step_holds_no_borrow_while_a_task_runs() {
    if child() {
        return;
    }
    let mut sim = Simulator::new(SimConfig::default());
    let _active = sim.activate();
    let polls = Arc::new(AtomicU32::new(0));
    let out = polls.clone();
    sim_ffi::spawn_rust_task("poller", 1, 65536, move |_| {
        for _ in 0..10 {
            unsafe { sim_ffi::sim_budget_poll(std::ptr::null(), 0) };
            out.fetch_add(1, Ordering::SeqCst);
        }
    });
    for _ in 0..100 {
        if unsafe { sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick() } == 0 {
            break;
        }
    }
    assert_eq!(polls.load(Ordering::SeqCst), 10);
}
