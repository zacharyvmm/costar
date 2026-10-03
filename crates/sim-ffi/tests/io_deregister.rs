//! Deregistering a descriptor ends every wait on it.
//!
//! A task blocked in `sim_host_block_on_fd()` on a descriptor that host
//! code then deregisters returns, without readiness: nothing could ever
//! report it ready.  The machine then completes instead of waiting for it
//! forever — on a FreeRTOS machine (the task waits in the kernel) as on a
//! native-only one, whether the scheduler is stepped or run to completion.
#![cfg(unix)]

use std::cell::Cell;
use std::ffi::{c_long, c_ulong, c_void};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;

extern "C" {
    fn xTaskCreate(
        entry: unsafe extern "C" fn(*mut c_void),
        name: *const std::ffi::c_char,
        depth: u16,
        arg: *mut c_void,
        priority: c_ulong,
        handle: *mut *mut c_void,
    ) -> c_long;
}

unsafe extern "C" fn anchor(_: *mut c_void) {}

thread_local! {
    static RETURNED: Cell<bool> = const { Cell::new(false) };
}

/// Returns (whether the waiter returned, whether the scheduler completed).
fn run(freertos: bool, run_to_completion: bool) -> (bool, bool) {
    RETURNED.with(|r| r.set(false));
    let (reader, _writer) = UnixStream::pair().unwrap();
    let fd = reader.as_raw_fd();
    let mut sim = Simulator::new(SimConfig::default());
    let _active = sim.activate();
    assert_eq!(unsafe { sim_ffi::net_ffi::sim_host_register_fd(fd) }, 0);
    if freertos {
        let created = unsafe {
            xTaskCreate(
                anchor,
                c"anchor".as_ptr(),
                256,
                std::ptr::null_mut(),
                1,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(created, 1);
    }
    sim_ffi::spawn_rust_task("waiter", 5, 65536, move |_| unsafe {
        sim_ffi::net_ffi::sim_host_block_on_fd(fd);
        RETURNED.with(|r| r.set(true));
    });
    sim.set_scheduler_limit(Some(0));
    unsafe { sim_ffi::sim_scheduler_tick() };
    assert!(!RETURNED.with(Cell::get), "the waiter did not block");
    assert_eq!(sim_ffi::net_ffi::sim_host_deregister_fd(fd), 0);
    sim.set_scheduler_limit(None);
    let completed = if run_to_completion {
        unsafe { sim_ffi::sim_start_scheduler() };
        true
    } else {
        (0..12).any(|_| unsafe { sim_ffi::sim_scheduler_tick() } == 0)
    };
    drop(reader);
    (RETURNED.with(Cell::get), completed)
}

#[test]
fn deregistering_a_descriptor_ends_the_wait_on_it() {
    for freertos in [true, false] {
        for run_to_completion in [false, true] {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || tx.send(run(freertos, run_to_completion)).unwrap());
            let (returned, completed) =
                rx.recv_timeout(Duration::from_secs(20))
                    .unwrap_or_else(|_| {
                        panic!(
                            "freertos={freertos} run_to_completion={run_to_completion}: \
                         the scheduler never completed"
                        )
                    });
            assert!(
                returned,
                "freertos={freertos} run_to_completion={run_to_completion}: the waiter never returned"
            );
            assert!(
                completed,
                "freertos={freertos} run_to_completion={run_to_completion}"
            );
        }
    }
}
