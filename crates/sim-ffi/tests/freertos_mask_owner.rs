//! The interrupt mask (and critical nesting) belongs to whoever began it,
//! and one release, keyed on that owner, serves every way a task retires.
//!
//! Matrix: mask owner {the retiring task, host code} × retirement {return,
//! fault, exit, self-delete, delete from a callback, delete by host code
//! between steps}, stepped bounded and unbounded.  A busy task (priority
//! 7) uses up its budget once (suspended, masked if it owns the mask);
//! host code masks then if it owns the mask; the task retires; a ready
//! peer (priority 6) waits.
//!
//! - The retiring task's own mask dies with it: interrupts are unmasked,
//!   the tick interrupts the mask held off are serviced (the kernel's tick
//!   count is current) and the peer runs.
//! - A host mask survives: interrupts stay masked and the peer waits for
//!   the host's unmask, then runs.
//!
//! Also: a callback that deletes the masked task and then starts a 5-tick
//! software timer sees the kernel's tick count current (the timer fires at
//! 15, not at 10).

use std::cell::{Cell, RefCell};
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
    fn xTaskGetTickCount() -> u32;
    fn xTimerCreate(
        name: *const c_char,
        period: u32,
        auto: c_long,
        id: *mut c_void,
        cb: unsafe extern "C" fn(*mut c_void),
    ) -> *mut c_void;
    fn xTimerGenericCommandFromTask(
        t: *mut c_void,
        cmd: c_long,
        value: u32,
        woken: *mut c_long,
        block: u32,
    ) -> c_long;
}

thread_local! {
    static HANDLE: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
    static FIRED: RefCell<Vec<(u64, u32)>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    Task,
    Host,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Retire {
    Return,
    Fault,
    Exit,
    SelfDelete,
    DeleteFromCallback,
    DeleteByHost,
}

const RETIRES: [Retire; 6] = [
    Retire::Return,
    Retire::Fault,
    Retire::Exit,
    Retire::SelfDelete,
    Retire::DeleteFromCallback,
    Retire::DeleteByHost,
];

unsafe extern "C" fn anchor(_: *mut c_void) {}

unsafe extern "C" fn delete_busy() {
    vTaskDelete(HANDLE.with(Cell::get));
}

/// Boot the active machine: an anchor, the peer and the busy task.
/// Returns the peer's "ran" flag.
fn setup(owner: Owner, retire: Retire) -> Arc<AtomicBool> {
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
    let peer_ran = Arc::new(AtomicBool::new(false));
    let peer = peer_ran.clone();
    sim_ffi::spawn_rust_task("peer", 6, 65536, move |_| {
        peer.store(true, Ordering::SeqCst);
    });
    sim_ffi::spawn_rust_task("busy", 7, 65536, move |_| unsafe {
        HANDLE.with(|h| h.set(xTaskGetCurrentTaskHandle()));
        if owner == Owner::Task {
            sim_ffi::sim_enter_critical();
        }
        // Use up the budget once: suspended (masked, if the task owns the
        // mask) until the next step.
        sim_ffi::sim_budget_set_limit(1);
        sim_ffi::sim_budget_reset();
        sim_ffi::sim_budget_poll(std::ptr::null(), 0);
        match retire {
            Retire::Return => {}
            Retire::Fault => panic!("the busy task faults"),
            Retire::Exit => loop {
                sim_ffi::sim_task_exit();
            },
            Retire::SelfDelete => vTaskDelete(std::ptr::null_mut()),
            // Busy until deleted.
            Retire::DeleteFromCallback | Retire::DeleteByHost => loop {
                sim_ffi::sim_budget_poll(std::ptr::null(), 0);
            },
        }
    });
    peer_ran
}

fn steps(sim: &mut Simulator, bounded: bool, limit: u64) {
    sim.set_scheduler_limit(bounded.then_some(limit));
    for _ in 0..30 {
        if unsafe { sim_ffi::sim_scheduler_tick() } == 0 && bounded {
            break;
        }
    }
}

fn kernel_tick_is_current(sim: &Simulator) -> bool {
    u64::from(unsafe { xTaskGetTickCount() }) == sim.scheduler_sim_time()
}

fn case(owner: Owner, retire: Retire, bounded: bool) {
    let case = format!("owner={owner:?} retire={retire:?} bounded={bounded}");
    let mut sim = Simulator::new(SimConfig::default());
    let _active = sim.activate();
    let peer_ran = setup(owner, retire);
    if retire == Retire::DeleteFromCallback {
        unsafe { sim_ffi::sim_schedule_event(2, Some(delete_busy)) };
    }
    // First step: the busy task starts and uses up its budget.
    sim.set_scheduler_limit(bounded.then_some(0));
    unsafe { sim_ffi::sim_scheduler_tick() };
    assert!(
        !peer_ran.load(Ordering::SeqCst),
        "{case}: the peer ran early"
    );
    if owner == Owner::Host {
        unsafe { sim_ffi::sim_enter_critical() };
    }
    assert!(sim_ffi::is_critical_locked(), "{case}");
    if retire == Retire::DeleteByHost {
        unsafe { delete_busy() };
    }
    steps(&mut sim, bounded, 5);
    match owner {
        Owner::Task => {
            assert!(
                !sim_ffi::is_critical_locked(),
                "{case}: the task's mask outlived it"
            );
            assert!(
                peer_ran.load(Ordering::SeqCst),
                "{case}: the peer never ran"
            );
            assert!(
                kernel_tick_is_current(&sim),
                "{case}: held ticks not serviced (kernel {} vs {})",
                unsafe { xTaskGetTickCount() },
                sim.scheduler_sim_time()
            );
        }
        Owner::Host => {
            assert!(
                sim_ffi::is_critical_locked(),
                "{case}: the host's mask did not survive the retirement"
            );
            assert!(
                !peer_ran.load(Ordering::SeqCst),
                "{case}: the peer ran before the host's unmask"
            );
            unsafe { sim_ffi::sim_exit_critical() };
            steps(&mut sim, bounded, 10);
            assert!(
                peer_ran.load(Ordering::SeqCst),
                "{case}: the peer never ran"
            );
        }
    }
    unsafe { sim_ffi::sim_budget_set_limit(1_000_000) };
}

#[test]
fn a_retiring_task_releases_only_its_own_mask() {
    let mut failed = Vec::new();
    for owner in [Owner::Task, Owner::Host] {
        for retire in RETIRES {
            for bounded in [false, true] {
                let ok = std::panic::catch_unwind(|| case(owner, retire, bounded));
                if ok.is_err() {
                    failed.push((owner, retire, bounded));
                }
            }
        }
    }
    assert!(failed.is_empty(), "failed: {failed:?}");
}

unsafe extern "C" fn fired(_: *mut c_void) {
    FIRED.with(|f| {
        f.borrow_mut()
            .push((sim_ffi::sim_now_ticks(), xTaskGetTickCount()))
    });
}

/// The callback deletes the masked task at tick 10, then starts a 5-tick
/// timer from the kernel's tick count.
unsafe extern "C" fn delete_and_start_timer() {
    vTaskDelete(HANDLE.with(Cell::get));
    assert!(!sim_ffi::is_critical_locked());
    let tick = xTaskGetTickCount();
    assert_eq!(u64::from(tick), sim_ffi::sim_now_ticks(), "held ticks");
    let timer = xTimerCreate(c"probe".as_ptr(), 5, 0, std::ptr::null_mut(), fired);
    assert!(!timer.is_null());
    assert_eq!(
        xTimerGenericCommandFromTask(timer, 1, tick, std::ptr::null_mut(), 0),
        1
    );
    sim_ffi::sim_budget_set_limit(1_000_000);
}

#[test]
fn releasing_a_deleted_tasks_mask_services_the_held_ticks() {
    for bounded in [false, true] {
        FIRED.with(|f| f.borrow_mut().clear());
        let mut sim = Simulator::new(SimConfig::default());
        let _active = sim.activate();
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
        sim_ffi::spawn_rust_task("busy", 7, 65536, move |_| unsafe {
            HANDLE.with(|h| h.set(xTaskGetCurrentTaskHandle()));
            sim_ffi::sim_enter_critical();
            sim_ffi::sim_budget_set_limit(1);
            sim_ffi::sim_budget_reset();
            loop {
                sim_ffi::sim_budget_poll(std::ptr::null(), 0);
            }
        });
        unsafe { sim_ffi::sim_schedule_event(10, Some(delete_and_start_timer)) };
        sim.set_scheduler_limit(bounded.then_some(20));
        for _ in 0..100 {
            if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
                break;
            }
        }
        assert_eq!(
            FIRED.with(|f| f.borrow().clone()),
            vec![(15, 15)],
            "bounded={bounded}"
        );
    }
}
