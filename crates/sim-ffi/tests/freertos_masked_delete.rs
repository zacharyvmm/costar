//! Host code deletes the selected task while interrupts are masked.
//!
//! The deleted TCB waits on FreeRTOS's termination list for the idle task
//! to free it: the engine must never suspend it (that took it off the list
//! without the kernel's bookkeeping, and the idle task's cleanup later
//! crashed).  FreeRTOS selects another task at once — the deleted one is
//! gone — but while interrupts stay masked that task does not run: the
//! machine idles masked, time keeps moving, and the task runs only after
//! the unmask, with interrupts unmasked.

use std::ffi::{c_char, c_long, c_ulong, c_void};
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
    fn vTaskDelete(task: *mut c_void);
}

unsafe extern "C" fn busy(_: *mut c_void) {
    sim_ffi::sim_budget_set_limit(1);
    loop {
        sim_ffi::sim_budget_poll(std::ptr::null(), 0);
    }
}

/// `(firmware tick, masked)` the peer first ran at, if it ran.
type Run = Option<(u64, bool)>;
type FirstRun = Arc<Mutex<Run>>;

/// A priority-7 busy task uses up its budget at bounded tick 0; host code
/// deletes it (masked, or not) and the machine is stepped to tick 3, then
/// unmasked and stepped on until the idle task has freed the TCB.
fn run(masked: bool) -> (Run, Run) {
    let mut sim = Simulator::new(SimConfig::default());
    let _active = sim.activate();
    let mut handle = std::ptr::null_mut();
    let created = unsafe {
        xTaskCreate(
            busy,
            c"busy".as_ptr(),
            256,
            std::ptr::null_mut(),
            7,
            &mut handle,
        )
    };
    assert_eq!(created, 1);
    let first: FirstRun = Arc::new(Mutex::new(None));
    let peer = first.clone();
    sim_ffi::spawn_rust_task("peer", 6, 65536, move |ctx| unsafe {
        peer.lock()
            .unwrap()
            .get_or_insert((ctx.now(), sim_ffi::is_critical_locked()));
        while ctx.now() < 6 {
            sim_ffi::sim_budget_poll(std::ptr::null(), 0);
        }
    });
    sim.set_scheduler_limit(Some(0));
    unsafe { sim_ffi::sim_scheduler_tick() };
    if masked {
        sim_ffi::freertos::sim_disable_interrupts();
    }
    unsafe { vTaskDelete(handle) };
    for tick in 1..=3 {
        sim.set_scheduler_limit(Some(tick));
        unsafe { sim_ffi::sim_scheduler_tick() };
    }
    let before_unmask = *first.lock().unwrap();
    if masked {
        sim_ffi::freertos::sim_enable_interrupts();
    }
    // Bounded, then unbounded: the idle task frees the deleted TCB.
    for tick in 3..=10 {
        sim.set_scheduler_limit(Some(tick));
        unsafe { sim_ffi::sim_scheduler_tick() };
    }
    sim.set_scheduler_limit(None);
    for _ in 0..20 {
        unsafe { sim_ffi::sim_scheduler_tick() };
    }
    let after = *first.lock().unwrap();
    (before_unmask, after)
}

#[test]
fn deleting_the_selected_task_while_masked_defers_the_next_task_to_the_unmask() {
    let (before_unmask, after) = run(true);
    // Nothing ran while masked; time kept moving to tick 3.
    assert_eq!(before_unmask, None, "a task ran before the host unmasked");
    // The peer ran after the unmask, unmasked, at tick 3.
    assert_eq!(after, Some((3, false)));
}

#[test]
fn deleting_the_selected_task_unmasked_runs_the_next_task_at_once() {
    let (before_unmask, after) = run(false);
    assert_eq!(before_unmask, Some((1, false)));
    assert_eq!(after, Some((1, false)));
}

/// Deletes itself inside a critical section, then keeps running masked
/// until a budget tick suspends it — as when a budget tick lands inside
/// `vTaskDelete()`'s own critical section after `traceTASK_DELETE`.
unsafe extern "C" fn delete_self_masked(_: *mut c_void) {
    sim_ffi::sim_enter_critical();
    vTaskDelete(std::ptr::null_mut());
    sim_ffi::sim_budget_set_limit(1);
    loop {
        sim_ffi::sim_budget_poll(std::ptr::null(), 0);
    }
}

/// The interrupt mask belongs to the task: retiring the deleted task
/// releases it, so the peer runs, unmasked, and the machine is not left
/// masked — bounded and unbounded.
#[test]
fn a_task_retired_while_holding_the_mask_releases_it() {
    for bounded in [true, false] {
        let mut sim = Simulator::new(SimConfig::default());
        let _active = sim.activate();
        let created = unsafe {
            xTaskCreate(
                delete_self_masked,
                c"deleter".as_ptr(),
                256,
                std::ptr::null_mut(),
                7,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(created, 1);
        let first: FirstRun = Arc::new(Mutex::new(None));
        let peer = first.clone();
        sim_ffi::spawn_rust_task("peer", 6, 65536, move |ctx| {
            peer.lock()
                .unwrap()
                .get_or_insert((ctx.now(), sim_ffi::is_critical_locked()));
        });
        sim.set_scheduler_limit(bounded.then_some(10));
        for _ in 0..20 {
            if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
                break;
            }
        }
        let ran = *first.lock().unwrap();
        assert!(
            ran.is_some_and(|(_, masked)| !masked),
            "bounded={bounded}: the peer did not run unmasked: {ran:?}"
        );
        assert!(!sim_ffi::is_critical_locked(), "bounded={bounded}");
    }
}
