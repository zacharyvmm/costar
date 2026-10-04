//! The interrupt mask (and critical nesting) belongs to whoever began it.
//! A task's mask dies with the task, whatever retires it: here a busy
//! task masks, uses up its budget (suspended, still masked) and is deleted
//! by a peripheral callback or by host code between steps.  Its ready peer
//! then runs, and interrupts are unmasked.  A mask host code began between
//! steps is the host's: deleting a task leaves it in place, and the
//! replacement waits for the host's unmask.

use std::cell::Cell;
use std::ffi::{c_char, c_long, c_ulong, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
    fn xTaskGetCurrentTaskHandle() -> *mut c_void;
    fn vTaskDelete(task: *mut c_void);
}

thread_local! {
    static HANDLE: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
}

unsafe extern "C" fn anchor(_: *mut c_void) {}

unsafe extern "C" fn delete_busy() {
    vTaskDelete(HANDLE.with(Cell::get));
}

/// An anchor, a ready peer (priority 6) and a busy task (priority 7) that
/// masks (if `mask`) and then burns its budget for ever, on the active
/// machine.  Returns the peer's "ran" flag.
fn setup(mask: bool) -> Arc<AtomicBool> {
    let peer_ran = Arc::new(AtomicBool::new(false));
    let peer = peer_ran.clone();
    {
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
        sim_ffi::spawn_rust_task("peer", 6, 65536, move |_| {
            peer.store(true, Ordering::SeqCst);
        });
        sim_ffi::spawn_rust_task("busy", 7, 65536, move |_| unsafe {
            HANDLE.with(|h| h.set(xTaskGetCurrentTaskHandle()));
            if mask {
                sim_ffi::sim_enter_critical();
            }
            sim_ffi::sim_budget_set_limit(1);
            sim_ffi::sim_budget_reset();
            loop {
                sim_ffi::sim_budget_poll(std::ptr::null(), 0);
            }
        });
    }
    peer_ran
}

fn step_until_done() {
    for _ in 0..20 {
        if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
            break;
        }
    }
}

#[test]
fn a_callback_deleting_the_masked_task_releases_its_mask() {
    for bounded in [false, true] {
        for mask in [true, false] {
            let case = format!("bounded={bounded} mask={mask}");
            let mut sim = Simulator::new(SimConfig::default());
            let _active = sim.activate();
            let peer_ran = setup(mask);
            unsafe { sim_ffi::sim_schedule_event(1, Some(delete_busy)) };
            sim.set_scheduler_limit(bounded.then_some(3));
            step_until_done();
            assert!(
                peer_ran.load(Ordering::SeqCst),
                "{case}: the peer never ran"
            );
            assert!(
                !sim_ffi::is_critical_locked(),
                "{case}: the mask outlived its task"
            );
            unsafe { sim_ffi::sim_budget_set_limit(1_000_000) };
        }
    }
}

#[test]
fn host_code_deleting_the_masked_task_between_steps_releases_its_mask() {
    for bounded in [false, true] {
        let case = format!("bounded={bounded}");
        let mut sim = Simulator::new(SimConfig::default());
        let _active = sim.activate();
        let peer_ran = setup(true);
        // The busy task runs, masks and uses up its budget.
        sim.set_scheduler_limit(bounded.then_some(1));
        for _ in 0..3 {
            unsafe { sim_ffi::sim_scheduler_tick() };
        }
        assert!(!peer_ran.load(Ordering::SeqCst), "{case}");
        assert!(
            sim_ffi::is_critical_locked(),
            "{case}: the busy task masked"
        );
        // Host code between steps.
        unsafe { delete_busy() };
        assert!(
            !sim_ffi::is_critical_locked(),
            "{case}: the mask outlived its task"
        );
        sim.set_scheduler_limit(bounded.then_some(5));
        step_until_done();
        assert!(
            peer_ran.load(Ordering::SeqCst),
            "{case}: the peer never ran"
        );
        unsafe { sim_ffi::sim_budget_set_limit(1_000_000) };
    }
}

#[test]
fn a_host_mask_survives_the_deletion_of_a_task() {
    for bounded in [false, true] {
        let case = format!("bounded={bounded}");
        let mut sim = Simulator::new(SimConfig::default());
        let _active = sim.activate();
        let peer_ran = setup(false);
        sim.set_scheduler_limit(bounded.then_some(1));
        for _ in 0..3 {
            unsafe { sim_ffi::sim_scheduler_tick() };
        }
        // Host code masks, then deletes the busy (selected) task.
        unsafe {
            sim_ffi::sim_enter_critical();
            delete_busy();
        }
        assert!(
            sim_ffi::is_critical_locked(),
            "{case}: the host's mask was dropped"
        );
        sim.set_scheduler_limit(bounded.then_some(5));
        step_until_done();
        assert!(
            !peer_ran.load(Ordering::SeqCst),
            "{case}: the replacement ran before the host's unmask"
        );
        assert!(sim_ffi::is_critical_locked(), "{case}");
        unsafe { sim_ffi::sim_exit_critical() };
        sim.set_scheduler_limit(bounded.then_some(10));
        step_until_done();
        assert!(
            peer_ran.load(Ordering::SeqCst),
            "{case}: the peer never ran"
        );
        unsafe { sim_ffi::sim_budget_set_limit(1_000_000) };
    }
}
