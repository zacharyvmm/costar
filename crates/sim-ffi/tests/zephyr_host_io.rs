//! A Zephyr thread run by `sim_zephyr_start_scheduler()` can wait on a host
//! descriptor: the scheduler does not hold the task table while it runs.

#![cfg(unix)]
use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;
use std::ffi::c_void;
use std::io::Write;
use std::os::fd::AsRawFd;
unsafe extern "C" fn worker(arg: *mut c_void, _: *mut c_void, _: *mut c_void) {
    sim_ffi::net_ffi::sim_host_block_on_fd(arg as usize as i32);
    sim_ffi::sim_trace_u32(c"zephyr_wait_done".as_ptr(), 1);
}
#[test]
fn zephyr_can_enter_host_io_wait() {
    let mut sim = Simulator::new(SimConfig::default());
    let g = sim.sim_global.clone();
    let _a = sim.activate();
    let (rx, mut tx) = std::os::unix::net::UnixStream::pair().unwrap();
    let fd = rx.as_raw_fd();
    assert_eq!(unsafe { sim_ffi::net_ffi::sim_host_register_fd(fd) }, 0);
    tx.write_all(b"x").unwrap();
    unsafe {
        sim_ffi::zephyr_ffi::sim_zephyr_register_thread(
            c"waiter".as_ptr(),
            Some(worker),
            fd as usize as *mut c_void,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            4096,
            5,
        );
    }
    unsafe { sim_ffi::sim_schedule_event(1, Some(wake)) };
    unsafe { sim_ffi::zephyr_ffi::sim_zephyr_start_scheduler() };
    assert_eq!(sim_ffi::net_ffi::sim_host_deregister_fd(fd), 0);
    let done = g.borrow().trace.as_ref().unwrap().events.iter().any(|e| {
        matches!(
            e,
            sim_core::TraceEvent::UserU32 {
                label: "zephyr_wait_done",
                ..
            }
        )
    });
    assert!(done, "the Zephyr thread's host I/O wait never ended");
}
unsafe extern "C" fn wake() {
    sim_ffi::host_poll_and_wake(1, None);
}
