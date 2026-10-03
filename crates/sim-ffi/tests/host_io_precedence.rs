//! A task waiting on a host descriptor that is already readable runs before
//! virtual time moves to a peripheral callback, on every scheduler: a chain
//! of callbacks (each scheduling the next for the following tick) must not
//! starve host I/O.

#![cfg(unix)]

use std::cell::Cell;
use std::ffi::{c_char, c_void};
use std::io::Write;
use std::os::fd::AsRawFd;

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;

extern "C" {
    fn costar_test_spawn_task(name: *const c_char, body: extern "C" fn(), priority: u32);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Native,
    FreeRtosStandalone,
    FreeRtosBounded,
    ZephyrLoop,
    ZephyrTick,
}

thread_local! {
    static FD: Cell<i32> = const { Cell::new(-1) };
    /// Tick at which the waiter's I/O wait ended.
    static DONE_AT: Cell<Option<u64>> = const { Cell::new(None) };
    /// Callbacks still to schedule (`u32::MAX`: never stop).
    static CHAIN_LEFT: Cell<u32> = const { Cell::new(0) };
}

extern "C" fn waiter() {
    unsafe { sim_ffi::net_ffi::sim_host_block_on_fd(FD.with(Cell::get)) };
    DONE_AT.with(|d| d.set(Some(unsafe { sim_ffi::sim_now_ticks() })));
}

unsafe extern "C" fn zephyr_waiter(_: *mut c_void, _: *mut c_void, _: *mut c_void) {
    waiter();
}

/// A peripheral callback that schedules the next one for the next tick.
unsafe extern "C" fn chain() {
    let left = CHAIN_LEFT.with(Cell::get);
    if left > 0 {
        if left != u32::MAX {
            CHAIN_LEFT.with(|c| c.set(left - 1));
        }
        sim_ffi::sim_schedule_event(sim_ffi::sim_now_ticks() + 1, Some(chain));
    }
}

/// Run a waiter on an already-readable socket next to a callback chain of
/// `callbacks` (from tick 1).  Returns the tick its wait ended at, if it
/// did within the step bound (and the scheduler returned in time).
fn waiter_done_at(backend: Backend, callbacks: u32) -> Option<u64> {
    let case = format!("{backend:?}/{callbacks}");
    let (done, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(case)
        .spawn(move || {
            let mut sim = Simulator::new(SimConfig::default());
            let _active = sim.activate();
            let (rx, mut tx) = std::os::unix::net::UnixStream::pair().unwrap();
            let fd = rx.as_raw_fd();
            FD.with(|f| f.set(fd));
            assert_eq!(unsafe { sim_ffi::net_ffi::sim_host_register_fd(fd) }, 0);
            tx.write_all(b"x").unwrap();
            CHAIN_LEFT.with(|c| c.set(callbacks));
            unsafe { sim_ffi::sim_schedule_event(1, Some(chain)) };
            match backend {
                Backend::Native => {
                    sim_ffi::spawn_rust_task("waiter", 1, 4096, |_| waiter());
                }
                Backend::FreeRtosStandalone | Backend::FreeRtosBounded => unsafe {
                    costar_test_spawn_task(c"waiter".as_ptr(), waiter, 1);
                },
                Backend::ZephyrLoop | Backend::ZephyrTick => unsafe {
                    sim_ffi::zephyr_ffi::sim_zephyr_register_thread(
                        c"waiter".as_ptr(),
                        Some(zephyr_waiter),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        4096,
                        5,
                    );
                },
            }
            if backend == Backend::ZephyrLoop {
                unsafe { sim_ffi::zephyr_ffi::sim_zephyr_start_scheduler() };
            } else {
                for step in 0..500u64 {
                    if DONE_AT.with(Cell::get).is_some() {
                        break;
                    }
                    if backend == Backend::FreeRtosBounded {
                        sim.set_scheduler_limit(Some(step));
                    }
                    let more = unsafe {
                        if backend == Backend::ZephyrTick {
                            sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick()
                        } else {
                            sim_ffi::sim_scheduler_tick()
                        }
                    };
                    if more == 0 && backend != Backend::FreeRtosBounded {
                        break;
                    }
                }
            }
            sim_ffi::net_ffi::sim_host_deregister_fd(fd);
            let _ = done.send(DONE_AT.with(Cell::get));
        })
        .unwrap();
    // A scheduler that never polls host I/O never returns: a failure.
    rx.recv_timeout(std::time::Duration::from_secs(30))
        .ok()
        .flatten()
}

const ALL: [Backend; 5] = [
    Backend::Native,
    Backend::FreeRtosStandalone,
    Backend::FreeRtosBounded,
    Backend::ZephyrLoop,
    Backend::ZephyrTick,
];

#[test]
fn ready_host_io_runs_before_time_moves_to_a_callback() {
    for backend in ALL {
        // Without callbacks, and with a finite chain of 100: the wait ends
        // at tick 0 either way, not after the chain (tick 101).
        assert_eq!(waiter_done_at(backend, 0), Some(0), "{backend:?}");
        assert_eq!(waiter_done_at(backend, 100), Some(0), "{backend:?}");
    }
}

#[test]
fn an_endless_callback_chain_never_starves_ready_host_io() {
    // The Zephyr loop runs until its threads finish and the callbacks run
    // out; an endless chain never runs out, so it has only the finite case.
    for backend in ALL.into_iter().filter(|&b| b != Backend::ZephyrLoop) {
        assert_eq!(waiter_done_at(backend, u32::MAX), Some(0), "{backend:?}");
    }
}
