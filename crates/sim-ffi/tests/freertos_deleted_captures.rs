//! Deleting a native task releases its stack outside the task table.
//!
//! A task that never ran still owns its closure: deleting it drops the
//! closure and everything it captured.  A captured value's destructor may
//! call the simulator (here, `spawn_rust_task()`); that must work, so the
//! engine never releases a deleted task's stack while it holds the task
//! table borrowed.  Covered for a task deleted before it ever ran, one
//! deleted while suspended (its stack is leaked, so its captures are never
//! dropped), and one that finished (its closure dropped its captures as it
//! ran).

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
    fn vTaskDelete(task: *mut c_void);
    fn xTaskGetCurrentTaskHandle() -> *mut c_void;
}

thread_local! {
    static DROPS: Cell<u32> = const { Cell::new(0) };
    static HANDLE: Cell<usize> = const { Cell::new(0) };
}

/// Calls back into the simulator when dropped.
struct ReentrantDrop;

impl Drop for ReentrantDrop {
    fn drop(&mut self) {
        DROPS.with(|d| d.set(d.get() + 1));
        sim_ffi::spawn_rust_task("cleanup", 1, 65536, |_| {});
    }
}

unsafe extern "C" fn busy(_: *mut c_void) {
    sim_ffi::sim_budget_set_limit(1);
    loop {
        sim_ffi::sim_budget_poll(std::ptr::null(), 0);
    }
}

fn cleanup_spawned(sim: &Simulator) -> bool {
    sim.sim_global
        .borrow()
        .tasks
        .iter()
        .any(|t| t.name == "cleanup")
}

/// The native task is adopted but never runs: a busy higher-priority task
/// holds the CPU, host code deletes it with interrupts masked (so the
/// native task, now selected, waits for the unmask), then deletes the
/// selected native task too.
#[test]
fn deleting_a_task_that_never_ran_drops_its_captures_outside_the_task_table() {
    DROPS.with(|d| d.set(0));
    let mut sim = Simulator::new(SimConfig::default());
    let _active = sim.activate();
    let mut busy_handle = std::ptr::null_mut();
    let created = unsafe {
        xTaskCreate(
            busy,
            c"busy".as_ptr(),
            256,
            std::ptr::null_mut(),
            7,
            &mut busy_handle,
        )
    };
    assert_eq!(created, 1);
    let capture = ReentrantDrop;
    let native = sim_ffi::spawn_rust_task("native", 6, 65536, move |_| drop(capture));
    sim.set_scheduler_limit(Some(0));
    unsafe { sim_ffi::sim_scheduler_tick() };
    sim_ffi::freertos::sim_disable_interrupts();
    unsafe { vTaskDelete(busy_handle) };
    sim.set_scheduler_limit(Some(1));
    unsafe { sim_ffi::sim_scheduler_tick() };
    let state = sim
        .sim_global
        .borrow()
        .tasks
        .iter()
        .find(|t| t.id == native)
        .unwrap()
        .state;
    assert_eq!(state, sim_fiber::TaskState::Created, "the task ran");
    let selected = unsafe { xTaskGetCurrentTaskHandle() };
    unsafe { vTaskDelete(selected) };
    assert_eq!(DROPS.with(Cell::get), 1, "its captures were not dropped");
    assert!(cleanup_spawned(&sim), "the destructor's spawn was lost");
    sim_ffi::freertos::sim_enable_interrupts();
    sim.set_scheduler_limit(None);
    for _ in 0..20 {
        if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
            break;
        }
    }
}

/// The native task ran and is blocked (sleeping in the kernel) when host
/// code deletes it: its stack is leaked, so its captures are never dropped,
/// and nothing goes wrong.
#[test]
fn deleting_a_suspended_task_leaks_its_captures_safely() {
    DROPS.with(|d| d.set(0));
    let mut sim = Simulator::new(SimConfig::default());
    let _active = sim.activate();
    unsafe extern "C" fn anchor(_: *mut c_void) {}
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
    let capture = ReentrantDrop;
    sim_ffi::spawn_rust_task("native", 6, 65536, move |ctx| {
        let _keep = &capture;
        HANDLE.with(|h| h.set(unsafe { xTaskGetCurrentTaskHandle() } as usize));
        ctx.sleep_for(100);
    });
    sim.set_scheduler_limit(Some(2));
    unsafe { sim_ffi::sim_scheduler_tick() };
    let handle = HANDLE.with(Cell::get);
    assert_ne!(handle, 0, "the task never ran");
    unsafe { vTaskDelete(handle as *mut c_void) };
    assert_eq!(DROPS.with(Cell::get), 0);
    sim.set_scheduler_limit(None);
    for _ in 0..20 {
        if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
            break;
        }
    }
}

/// The native task ran to completion: its closure dropped its captures as
/// it returned (on the task's fiber), and the engine's deletion of the
/// finished task releases its stack.
#[test]
fn a_finished_task_drops_its_captures_once() {
    DROPS.with(|d| d.set(0));
    let mut sim = Simulator::new(SimConfig::default());
    let _active = sim.activate();
    unsafe extern "C" fn anchor(_: *mut c_void) {}
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
    let capture = ReentrantDrop;
    sim_ffi::spawn_rust_task("native", 6, 65536, move |_| {
        let _keep = &capture;
    });
    sim.set_scheduler_limit(None);
    for _ in 0..20 {
        if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
            break;
        }
    }
    assert_eq!(DROPS.with(Cell::get), 1);
    assert!(cleanup_spawned(&sim));
}
