//! A peripheral callback that dispatches the callbacks due now itself
//! (`dispatch_events()`, say to flush other peripheral work) does not
//! recurse: the dispatch already running for the machine drains the queue
//! one callback at a time, and each is charged to the storm limit before
//! it runs.  A burst past the limit stops the machine; a finite burst
//! within a raised limit completes without overflowing the stack.

use std::cell::Cell;
use std::ffi::{c_char, c_long, c_ulong, c_void};

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;

extern "C" {
    fn xTaskCreate(
        f: unsafe extern "C" fn(*mut c_void),
        name: *const c_char,
        depth: u16,
        arg: *mut c_void,
        prio: c_ulong,
        out: *mut *mut c_void,
    ) -> c_long;
}

thread_local! {
    static CALLS: Cell<u32> = const { Cell::new(0) };
}

unsafe extern "C" fn anchor(_: *mut c_void) {}

unsafe extern "C" fn flushing_callback() {
    CALLS.with(|c| c.set(c.get() + 1));
    sim_ffi::dispatch_events(sim_ffi::sim_now_ticks());
}

/// `count` flushing callbacks due at tick 0, with storm limit `limit`.
/// Returns (halted, callbacks run).
fn run_batch(count: usize, limit: u32, freertos: bool) -> (bool, u32) {
    CALLS.with(|c| c.set(0));
    let mut sim = Simulator::new(SimConfig::default());
    sim.enable_owned_devices();
    sim.set_storm_limit(limit);
    let _active = sim.activate();
    if freertos {
        unsafe {
            assert_eq!(
                xTaskCreate(
                    anchor,
                    c"anchor".as_ptr(),
                    128,
                    std::ptr::null_mut(),
                    1,
                    std::ptr::null_mut()
                ),
                1
            );
        }
    }
    for _ in 0..count {
        unsafe { sim_ffi::sim_schedule_event(0, Some(flushing_callback)) };
    }
    for _ in 0..10 {
        if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
            break;
        }
    }
    (sim.halted(), CALLS.with(Cell::get))
}

#[test]
fn nested_dispatch_obeys_the_storm_limit() {
    for freertos in [false, true] {
        let (halted, calls) = run_batch(20, 4, freertos);
        assert!(halted, "freertos={freertos}: no storm");
        assert!(
            calls <= 5,
            "freertos={freertos}: limit 4 ran {calls} callbacks"
        );
    }
}

#[test]
fn a_finite_nested_batch_within_a_raised_limit_completes() {
    // On a thread with the default test stack: recursion would overflow.
    std::thread::spawn(|| {
        for freertos in [false, true] {
            let (halted, calls) = run_batch(20_000, 30_000, freertos);
            assert!(!halted, "freertos={freertos}: a finite batch stormed");
            assert_eq!(calls, 20_000, "freertos={freertos}");
        }
    })
    .join()
    .unwrap();
}
