//! Setting up a wait is one step with blocking in it.
//!
//! Under edge instrumentation (`SIM_INSTRUMENT_EDGES=1`) the kernel's own
//! functions poll the CPU budget, so a budget tick can land anywhere
//! inside `sim_host_block_on_fd()` or a sleep: between registering what
//! ends the wait and the task actually suspending.  The engine defers that
//! preemption until the wait has committed, so no peripheral callback can
//! run in between.  Otherwise a callback that deregisters the descriptor
//! (ending the wait) found the task not suspended yet, its wake-up was
//! lost, and the task then blocked for good; and a sleep could start from
//! a stale tick and wake late.
//!
//! The sweeps move the budget tick through every point of the setup
//! (`pad` kernel calls before it).  `tests/golden_trace_test.sh` runs them
//! with `SIM_INSTRUMENT_EDGES=1`; without instrumentation they still check
//! the plain behaviour.
#![cfg(unix)]

use std::cell::Cell;
use std::ffi::{c_char, c_long, c_ulong, c_void};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;

thread_local! {
    static FD: Cell<i32> = const { Cell::new(-1) };
}

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

unsafe extern "C" fn anchor(_: *mut c_void) {}

unsafe extern "C" fn cancel() {
    FD.with(|fd| assert_eq!(sim_ffi::net_ffi::sim_host_deregister_fd(fd.get()), 0));
}

fn freertos_machine() -> Simulator {
    Simulator::new(SimConfig::default())
}

fn make_anchor() {
    let created = unsafe {
        xTaskCreate(
            anchor,
            c"anchor".as_ptr(),
            128,
            std::ptr::null_mut(),
            1,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(created, 1);
}

/// Burn `pad` kernel calls (each an instrumented function entry).
fn pad_calls(pad: u32) {
    for _ in 0..pad {
        std::hint::black_box(unsafe { xTaskGetTickCount() });
    }
}

#[test]
fn a_wait_cancelled_during_its_setup_still_ends() {
    for pad in 0..1000 {
        let (reader, _writer) = UnixStream::pair().unwrap();
        let fd = reader.as_raw_fd();
        FD.with(|f| f.set(fd));
        let mut sim = freertos_machine();
        let _active = sim.activate();
        unsafe {
            sim_ffi::sim_budget_set_limit(1_000_000);
            sim_ffi::sim_budget_reset();
            assert_eq!(sim_ffi::net_ffi::sim_host_register_fd(fd), 0);
        }
        make_anchor();
        let done = Arc::new(AtomicBool::new(false));
        let out = done.clone();
        sim_ffi::spawn_rust_task("waiter", 7, 65536, move |_| unsafe {
            sim_ffi::sim_budget_set_limit(1_000_000);
            sim_ffi::sim_budget_reset();
            pad_calls(pad);
            // Deregisters the descriptor at the next tick: under
            // instrumentation, the budget tick lands inside the wait setup.
            sim_ffi::sim_schedule_event(sim_ffi::sim_now_ticks() + 1, Some(cancel));
            sim_ffi::sim_budget_set_limit(1);
            sim_ffi::sim_budget_reset();
            sim_ffi::net_ffi::sim_host_block_on_fd(fd);
            sim_ffi::sim_budget_set_limit(1_000_000);
            out.store(true, Ordering::SeqCst);
        });
        sim.set_scheduler_limit(Some(100));
        let more = unsafe { sim_ffi::sim_scheduler_tick() };
        assert!(
            done.load(Ordering::SeqCst),
            "pad={pad}: the cancelled wait did not end: more={more}, now={}, registrations={}",
            sim.scheduler_sim_time(),
            sim_ffi::freertos::io_registrations()
        );
        assert_eq!(sim_ffi::freertos::io_registrations(), 0, "pad={pad}");
        unsafe { sim_ffi::sim_budget_set_limit(1_000_000) };
    }
}

#[test]
fn a_sleep_preempted_during_its_setup_wakes_on_time() {
    for pad in 0..300 {
        let mut sim = freertos_machine();
        let _active = sim.activate();
        unsafe {
            sim_ffi::sim_budget_set_limit(1_000_000);
            sim_ffi::sim_budget_reset();
        }
        make_anchor();
        let woke = Arc::new(AtomicU64::new(u64::MAX));
        let out = woke.clone();
        sim_ffi::spawn_rust_task("sleeper", 7, 65536, move |ctx| unsafe {
            sim_ffi::sim_budget_set_limit(1_000_000);
            sim_ffi::sim_budget_reset();
            pad_calls(pad);
            sim_ffi::sim_budget_set_limit(1);
            sim_ffi::sim_budget_reset();
            ctx.sleep_until(10);
            sim_ffi::sim_budget_set_limit(1_000_000);
            out.store(ctx.now(), Ordering::SeqCst);
        });
        sim.set_scheduler_limit(Some(100));
        unsafe { sim_ffi::sim_scheduler_tick() };
        assert_eq!(woke.load(Ordering::SeqCst), 10, "pad={pad}");
        unsafe { sim_ffi::sim_budget_set_limit(1_000_000) };
    }
}
