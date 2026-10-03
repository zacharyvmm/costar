//! Waits that began before FreeRTOS adopted a task continue after it:
//! a delay keeps its deadline and a host I/O wait keeps waiting for
//! readiness, whether the machine is stepped bounded (World) or not.

use sim_core::{trace::TraceEvent, SimConfig};
use sim_ffi::simulator::Simulator;
use std::ffi::c_void;
extern "C" {
    fn costar_test_abi_delay_boot();
}
unsafe extern "C" fn sleeper(_: *mut c_void) {
    sim_ffi::sim_task_delay_until(10);
    sim_ffi::sim_trace_u32(
        c"preboot_delay_done".as_ptr(),
        sim_ffi::sim_now_ticks() as u32,
    );
}
fn c_sleep_case(world: bool) {
    let mut sim = Simulator::new(SimConfig::default());
    let g = sim.sim_global.clone();
    let _a = sim.activate();
    unsafe {
        sim_ffi::sim_create_task(
            c"sleeper".as_ptr(),
            Some(sleeper),
            std::ptr::null_mut(),
            128,
            5,
        );
        sim_ffi::sim_scheduler_tick(); // Native task has entered its sleep at tick 0.
        costar_test_abi_delay_boot();
    }
    g.borrow_mut().scheduler_limit = world.then_some(0);
    unsafe {
        sim_ffi::sim_scheduler_tick();
    }
    let done: Vec<_> = g
        .borrow()
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 {
                at,
                label: "preboot_delay_done",
                value,
            } => Some((*at, *value)),
            _ => None,
        })
        .collect();
    assert!(done.is_empty(), "sleep returned before tick 10: {done:?}");

    // The delay still ends at its deadline under FreeRTOS.
    for step in 1..100u64 {
        if world {
            g.borrow_mut().scheduler_limit = Some(step);
        }
        unsafe { sim_ffi::sim_scheduler_tick() };
        if g.borrow().scheduler_sim_time > 12 {
            break;
        }
    }
    let done: Vec<_> = g
        .borrow()
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 {
                at,
                label: "preboot_delay_done",
                value,
            } => Some((*at, *value)),
            _ => None,
        })
        .collect();
    assert_eq!(done, vec![(10, 10)], "world={world}");
}
#[cfg(unix)]
fn io_wait_case(world: bool) {
    use std::os::{fd::AsRawFd, unix::net::UnixStream};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let (reader, mut writer) = UnixStream::pair().unwrap();
    let mut sim = Simulator::new(SimConfig::default());
    let g = sim.sim_global.clone();
    let _a = sim.activate();
    let fd = reader.as_raw_fd();
    assert_eq!(unsafe { sim_ffi::net_ffi::sim_host_register_fd(fd) }, 0);
    let returned = Arc::new(AtomicBool::new(false));
    let copy = returned.clone();
    sim_ffi::spawn_rust_task("io_wait", 5, 4096, move |_| {
        unsafe {
            sim_ffi::net_ffi::sim_host_block_on_fd(fd);
        }
        copy.store(true, Ordering::SeqCst);
    });
    unsafe {
        sim_ffi::sim_scheduler_tick();
        costar_test_abi_delay_boot();
    }
    g.borrow_mut().scheduler_limit = world.then_some(0);
    unsafe {
        sim_ffi::sim_scheduler_tick();
    }
    assert!(
        !returned.load(Ordering::SeqCst),
        "I/O wait returned with no bytes written to socket"
    );

    // Once the descriptor is readable the wait ends under FreeRTOS.
    use std::io::Write;
    writer.write_all(b"x").unwrap();
    for step in 1..50u64 {
        if world {
            g.borrow_mut().scheduler_limit = Some(step);
        }
        unsafe { sim_ffi::sim_scheduler_tick() };
        if returned.load(Ordering::SeqCst) {
            break;
        }
    }
    sim_ffi::net_ffi::sim_host_deregister_fd(fd);
    assert!(
        returned.load(Ordering::SeqCst),
        "world={world}: readiness did not end the wait"
    );
}
#[test]
fn c_sleep_without_adoption_waits_until_deadline() {
    let mut sim = Simulator::new(SimConfig::default());
    let g = sim.sim_global.clone();
    let _a = sim.activate();
    unsafe {
        sim_ffi::sim_create_task(
            c"sleeper".as_ptr(),
            Some(sleeper),
            std::ptr::null_mut(),
            128,
            5,
        );
        for _ in 0..4 {
            sim_ffi::sim_scheduler_tick();
        }
    }
    let done: Vec<_> = g
        .borrow()
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 {
                at,
                label: "preboot_delay_done",
                value,
            } => Some((*at, *value)),
            _ => None,
        })
        .collect();
    assert_eq!(done, vec![(10, 10)]);
}

#[test]
fn c_sleep_survives_adoption_bounded() {
    c_sleep_case(true);
}
#[test]
fn c_sleep_survives_adoption_unbounded() {
    c_sleep_case(false);
}
#[test]
#[cfg(unix)]
fn native_io_wait_survives_adoption_bounded() {
    io_wait_case(true);
}
#[test]
#[cfg(unix)]
fn native_io_wait_survives_adoption_unbounded() {
    io_wait_case(false);
}
