//! Several tasks may wait on one host descriptor: readiness wakes every
//! one of them, and deregistering the descriptor ends every wait — on
//! FreeRTOS and native machines.
#![cfg(unix)]

use std::ffi::{c_char, c_long, c_ulong, c_void};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

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
}

unsafe extern "C" fn anchor(_: *mut c_void) {}

#[derive(Clone, Copy, Debug)]
enum End {
    Deregister,
    Ready,
}

fn run(freertos: bool, waiters: u32, end: End) -> (Vec<u32>, usize) {
    let (reader, mut writer) = UnixStream::pair().unwrap();
    let fd = reader.as_raw_fd();
    let mut sim = Simulator::new(SimConfig::default());
    let _active = sim.activate();
    assert_eq!(unsafe { sim_ffi::net_ffi::sim_host_register_fd(fd) }, 0);
    if freertos {
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
    let returned = Arc::new(Mutex::new(Vec::new()));
    for id in 0..waiters {
        let out = returned.clone();
        sim_ffi::spawn_rust_task("waiter", 6 - id, 65536, move |_| {
            unsafe { sim_ffi::net_ffi::sim_host_block_on_fd(fd) };
            out.lock().unwrap().push(id);
        });
    }
    sim.set_scheduler_limit(Some(0));
    for _ in 0..waiters + 1 {
        unsafe { sim_ffi::sim_scheduler_tick() };
    }
    assert!(
        returned.lock().unwrap().is_empty(),
        "a waiter did not block"
    );
    match end {
        End::Deregister => assert_eq!(sim_ffi::net_ffi::sim_host_deregister_fd(fd), 0),
        End::Ready => writer.write_all(b"x").unwrap(),
    }
    for tick in 1..=5 {
        sim.set_scheduler_limit(Some(tick));
        for _ in 0..waiters + 1 {
            unsafe { sim_ffi::sim_scheduler_tick() };
        }
    }
    if let End::Ready = end {
        assert_eq!(sim_ffi::net_ffi::sim_host_deregister_fd(fd), 0);
    }
    let mut ids = returned.lock().unwrap().clone();
    ids.sort();
    (ids, sim_ffi::freertos::io_registrations())
}

#[test]
fn every_waiter_on_a_descriptor_returns() {
    for freertos in [true, false] {
        for waiters in [2, 3] {
            for end in [End::Deregister, End::Ready] {
                let (ids, registrations) = run(freertos, waiters, end);
                let case = format!("freertos={freertos} waiters={waiters} {end:?}");
                assert_eq!(ids, (0..waiters).collect::<Vec<_>>(), "{case}");
                assert_eq!(registrations, 0, "{case}: registrations left");
            }
        }
    }
}
