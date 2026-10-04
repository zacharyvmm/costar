//! A task's engine registrations end with its wait or with the task.
//!
//! - Deregistering a descriptor ends the wait on it for good, even if the
//!   descriptor is registered again before the waiter resumes.
//! - A task whose fiber stops for good (a failed `configASSERT()`, an
//!   exit) or that is deleted leaves no registration behind — no kernel
//!   I/O wait, poller association, readiness or cancellation latch — so the
//!   machine goes quiescent and later readiness never revives it.
#![cfg(unix)]

use std::ffi::{c_char, c_long, c_ulong, c_void};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

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
    fn vTaskSuspendAll();
    fn vTaskDelete(task: *mut c_void);
    fn xTaskGetCurrentTaskHandle() -> *mut c_void;
}

unsafe extern "C" fn anchor(_: *mut c_void) {}

fn make_freertos() {
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

/// Steps until the machine reports nothing more (at most 20 steps);
/// returns whether it did.
fn quiesces(sim: &Simulator, bounded: bool) -> bool {
    for tick in 1..=20 {
        sim.set_scheduler_limit(bounded.then_some(tick));
        if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
            return true;
        }
    }
    false
}

#[test]
fn reregistering_a_descriptor_does_not_revive_a_cancelled_wait() {
    for freertos in [true, false] {
        let (reader, _writer) = UnixStream::pair().unwrap();
        let fd = reader.as_raw_fd();
        let mut sim = Simulator::new(SimConfig::default());
        let _active = sim.activate();
        assert_eq!(unsafe { sim_ffi::net_ffi::sim_host_register_fd(fd) }, 0);
        if freertos {
            make_freertos();
        }
        let returned = Arc::new(AtomicBool::new(false));
        let flag = returned.clone();
        sim_ffi::spawn_rust_task("waiter", 5, 65536, move |_| {
            unsafe { sim_ffi::net_ffi::sim_host_block_on_fd(fd) };
            flag.store(true, Ordering::SeqCst);
        });
        sim.set_scheduler_limit(Some(0));
        unsafe { sim_ffi::sim_scheduler_tick() };
        assert!(!returned.load(Ordering::SeqCst));
        assert_eq!(sim_ffi::net_ffi::sim_host_deregister_fd(fd), 0);
        assert_eq!(unsafe { sim_ffi::net_ffi::sim_host_register_fd(fd) }, 0);
        for tick in 1..=3 {
            sim.set_scheduler_limit(Some(tick));
            unsafe { sim_ffi::sim_scheduler_tick() };
        }
        assert!(
            returned.load(Ordering::SeqCst),
            "freertos={freertos}: the cancelled wait was revived"
        );
        assert!(quiesces(&sim, true), "freertos={freertos}");
        assert_eq!(
            sim_ffi::freertos::io_registrations(),
            0,
            "freertos={freertos}"
        );
        assert_eq!(sim_ffi::net_ffi::sim_host_deregister_fd(fd), 0);
    }
}

#[derive(Clone, Copy, Debug)]
enum Stop {
    /// A failed `configASSERT()`: the wait is entered holding the
    /// scheduler lock.
    Fault,
    /// The task ends after its wait.
    Exit,
    /// Host code deletes the waiting task.
    Delete,
}

#[derive(Clone, Copy, Debug)]
enum Wait {
    Io,
    Sleep,
}

fn retire_case(stop: Stop, wait: Wait, bounded: bool) {
    let case = format!("{stop:?} {wait:?} bounded={bounded}");
    let (reader, mut writer) = UnixStream::pair().unwrap();
    let fd = reader.as_raw_fd();
    let mut sim = Simulator::new(SimConfig::default());
    let _active = sim.activate();
    assert_eq!(unsafe { sim_ffi::net_ffi::sim_host_register_fd(fd) }, 0);
    make_freertos();
    let after = Arc::new(AtomicBool::new(false));
    let handle = Arc::new(AtomicUsize::new(0));
    let (after_flag, handle_out) = (after.clone(), handle.clone());
    sim_ffi::spawn_rust_task("stopping", 5, 65536, move |ctx| unsafe {
        handle_out.store(xTaskGetCurrentTaskHandle() as usize, Ordering::SeqCst);
        if let Stop::Fault = stop {
            vTaskSuspendAll();
        }
        match wait {
            Wait::Io => sim_ffi::net_ffi::sim_host_block_on_fd(fd),
            Wait::Sleep => ctx.sleep_for(3),
        }
        if let Stop::Exit = stop {
            sim_ffi::sim_task_exit();
        }
        after_flag.store(true, Ordering::SeqCst);
    });
    let healthy = Arc::new(AtomicBool::new(false));
    let healthy_flag = healthy.clone();
    sim_ffi::spawn_rust_task("healthy", 4, 65536, move |_| {
        healthy_flag.store(true, Ordering::SeqCst);
    });
    sim.set_scheduler_limit(Some(0));
    unsafe { sim_ffi::sim_scheduler_tick() };
    match stop {
        Stop::Delete => unsafe { vTaskDelete(handle.load(Ordering::SeqCst) as *mut c_void) },
        // Readiness for the exiting I/O waiter, so its wait completes.
        Stop::Exit => {
            writer.write_all(b"x").unwrap();
        }
        Stop::Fault => {}
    }
    assert!(
        quiesces(&sim, bounded),
        "{case}: the machine never went quiescent"
    );
    assert!(healthy.load(Ordering::SeqCst), "{case}");
    assert_eq!(
        sim_ffi::freertos::io_registrations(),
        0,
        "{case}: registrations left"
    );
    // Readiness (again) never revives a stopped task.
    writer.write_all(b"y").unwrap();
    assert!(quiesces(&sim, bounded), "{case}: revived");
    assert!(
        !after.load(Ordering::SeqCst),
        "{case}: the stopped task ran on"
    );
    assert_eq!(sim_ffi::net_ffi::sim_host_deregister_fd(fd), 0);
}

#[test]
fn a_stopped_or_deleted_task_leaves_no_registration() {
    for stop in [Stop::Fault, Stop::Exit, Stop::Delete] {
        for wait in [Wait::Io, Wait::Sleep] {
            for bounded in [true, false] {
                retire_case(stop, wait, bounded);
            }
        }
    }
}
