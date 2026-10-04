//! A running task creating a child with the legacy pattern
//! (`xTaskCreate()`, then a matching `sim_create_task()`).
//!
//! Pairing the two halves needs kernel constants (the name length FreeRTOS
//! keeps, the priority clamp).  Under edge instrumentation
//! (`SIM_INSTRUMENT_EDGES=1`) every C function polls the CPU budget, so a
//! budget tick can land inside any of them: the fiber suspends there.  If
//! that happens while the engine holds its task table borrowed, the
//! scheduler then aborts ("RefCell already borrowed").  The engine reads
//! everything it needs from C before borrowing.
//!
//! The sweep moves the budget tick through every point of the creation
//! (`pad` kernel calls before it; the edge hook polls every 10 000 edges).
//! `tests/golden_trace_test.sh` runs it with `SIM_INSTRUMENT_EDGES=1`;
//! without instrumentation a short sweep still checks that the pair is one
//! task.

use std::cell::Cell;
use std::ffi::{c_char, c_long, c_ulong, c_void};

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;

extern "C" {
    fn xTaskCreate(
        entry: unsafe extern "C" fn(*mut c_void),
        name: *const c_char,
        depth: u16,
        arg: *mut c_void,
        priority: c_ulong,
        handle: *mut *mut c_void,
    ) -> c_long;
    fn xTaskGetTickCount() -> u32;
}

thread_local! {
    static CHILD_RUNS: Cell<u32> = const { Cell::new(0) };
    static PAIRED: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "C" fn anchor(_: *mut c_void) {}

unsafe extern "C" fn child(_: *mut c_void) {
    CHILD_RUNS.with(|r| r.set(r.get() + 1));
}

unsafe extern "C" fn parent(arg: *mut c_void) {
    let pad = arg as usize;
    sim_ffi::sim_budget_set_limit(1_000_000);
    sim_ffi::sim_budget_reset();
    let created = xTaskCreate(
        child,
        c"child".as_ptr(),
        128,
        std::ptr::null_mut(),
        1,
        std::ptr::null_mut(),
    );
    assert_eq!(created, 1);
    for _ in 0..pad {
        std::hint::black_box(xTaskGetTickCount());
    }
    // The next budget poll (under instrumentation) preempts the task:
    // somewhere inside `sim_create_task()`, depending on `pad`.
    sim_ffi::sim_budget_set_limit(1);
    sim_ffi::sim_budget_reset();
    let handle =
        sim_ffi::sim_create_task(c"child".as_ptr(), Some(child), std::ptr::null_mut(), 128, 1);
    sim_ffi::sim_budget_set_limit(1_000_000);
    PAIRED.with(|p| p.set(handle != 0));
}

fn instrumented() -> bool {
    std::env::var_os("SIM_INSTRUMENT_EDGES").is_some_and(|v| v == "1")
}

#[test]
fn a_running_task_creating_a_legacy_pair_under_instrumentation() {
    let pads = if instrumented() { 4000 } else { 50 };
    for pad in 0..pads {
        CHILD_RUNS.with(|r| r.set(0));
        PAIRED.with(|p| p.set(false));
        let mut sim = Simulator::new(SimConfig::default());
        let _active = sim.activate();
        unsafe {
            sim_ffi::sim_budget_set_limit(1_000_000);
            sim_ffi::sim_budget_reset();
            xTaskCreate(
                anchor,
                c"anchor".as_ptr(),
                128,
                std::ptr::null_mut(),
                1,
                std::ptr::null_mut(),
            );
            xTaskCreate(
                parent,
                c"parent".as_ptr(),
                256,
                pad as *mut c_void,
                7,
                std::ptr::null_mut(),
            );
        }
        sim.set_scheduler_limit(Some(20));
        unsafe { sim_ffi::sim_scheduler_tick() };
        unsafe { sim_ffi::sim_budget_set_limit(1_000_000) };
        assert!(
            PAIRED.with(Cell::get),
            "pad={pad}: sim_create_task() failed"
        );
        assert_eq!(
            CHILD_RUNS.with(Cell::get),
            1,
            "pad={pad}: the legacy pair must be one task"
        );
    }
}
