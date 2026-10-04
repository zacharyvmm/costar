//! One owner-keyed release for a retiring task's interrupt state, on every
//! backend and every retirement path.
//!
//! Matrix: mask owner {the retiring task, an ISR, host code} × retirement
//! {return, fault, exit; on FreeRTOS also self-delete, delete from a
//! callback, delete by host code} × backend {native step, Zephyr step,
//! Zephyr loop, FreeRTOS standalone and bounded}.  The task (or host code)
//! masks, IRQ 7 is raised and held, the task retires:
//!
//! - its own mask dies with it: IRQ 7 is delivered, interrupts unmasked;
//! - an ISR's or the host's mask survives: IRQ 7 waits, interrupts stay
//!   masked, until the owner (here host code) unmasks; then IRQ 7 runs.
//!
//! (A mask owned by another task cannot exist here: a masked task keeps
//! the CPU until it unmasks or retires.)

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_long, c_ulong, c_void};

use sim_core::SimConfig;
use sim_ffi::device_ffi::{sim_irq_raise, sim_irq_set_handler};
use sim_ffi::freertos::{sim_disable_interrupts, sim_enable_interrupts};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    Task,
    Isr,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    NativeStep,
    ZephyrStep,
    ZephyrLoop,
    FreeRtos,
    FreeRtosBounded,
}

impl Backend {
    fn freertos(self) -> bool {
        matches!(self, Backend::FreeRtos | Backend::FreeRtosBounded)
    }
}

thread_local! {
    static DELIVERED: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    static HANDLE: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
}

unsafe extern "C" fn isr6() {
    DELIVERED.with(|d| d.borrow_mut().push(6));
    sim_disable_interrupts();
}

unsafe extern "C" fn isr7() {
    DELIVERED.with(|d| d.borrow_mut().push(7));
}

unsafe extern "C" fn anchor(_: *mut c_void) {}

unsafe extern "C" fn delete_task() {
    vTaskDelete(HANDLE.with(Cell::get));
}

fn delivered() -> Vec<u32> {
    DELIVERED.with(|d| d.borrow().clone())
}

/// The retiring task's body.
fn body(owner: Owner, retire: Retire, freertos: bool) {
    unsafe {
        if freertos {
            HANDLE.with(|h| h.set(xTaskGetCurrentTaskHandle()));
        }
        if owner == Owner::Host {
            // Let host code mask between steps: use up the budget (FreeRTOS:
            // suspended until the next tick) or yield (native).
            sim_ffi::sim_budget_set_limit(1);
            sim_ffi::sim_budget_reset();
            sim_ffi::sim_budget_poll(std::ptr::null(), 0);
            sim_ffi::sim_budget_set_limit(1_000_000);
            if !freertos {
                sim_ffi::sim_port_yield();
            }
            assert!(sim_ffi::is_critical_locked(), "the host masked");
        }
        match owner {
            Owner::Task => sim_ffi::sim_enter_critical(),
            // Delivered on this task's fiber: the ISR masks.
            Owner::Isr => sim_irq_raise(6),
            Owner::Host => {}
        }
        sim_irq_raise(7);
        match retire {
            Retire::Return => {}
            Retire::Fault => panic!("the task faults"),
            Retire::Exit => loop {
                sim_ffi::sim_task_exit();
            },
            Retire::SelfDelete => vTaskDelete(std::ptr::null_mut()),
            Retire::DeleteFromCallback | Retire::DeleteByHost => {
                sim_ffi::sim_budget_set_limit(1);
                sim_ffi::sim_budget_reset();
                loop {
                    sim_ffi::sim_budget_poll(std::ptr::null(), 0);
                }
            }
        }
    }
}

fn step(backend: Backend, sim: &mut Simulator, limit: u64) -> u32 {
    match backend {
        Backend::ZephyrStep => unsafe { sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick() },
        Backend::FreeRtosBounded => {
            sim.set_scheduler_limit(Some(limit));
            unsafe { sim_ffi::sim_scheduler_tick() }
        }
        _ => unsafe { sim_ffi::sim_scheduler_tick() },
    }
}

fn run(backend: Backend, sim: &mut Simulator, limit: u64) {
    if backend == Backend::ZephyrLoop {
        unsafe { sim_ffi::zephyr_ffi::sim_zephyr_start_scheduler() };
        return;
    }
    for _ in 0..20 {
        step(backend, sim, limit);
    }
}

fn case(owner: Owner, retire: Retire, backend: Backend) {
    let case = format!("owner={owner:?} retire={retire:?} backend={backend:?}");
    DELIVERED.with(|d| d.borrow_mut().clear());
    let mut sim = Simulator::new(SimConfig::default());
    sim.enable_owned_devices();
    let _active = sim.activate();
    let freertos = backend.freertos();
    unsafe {
        sim_irq_set_handler(6, Some(isr6));
        sim_irq_set_handler(7, Some(isr7));
        if freertos {
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
    }
    sim_ffi::spawn_rust_task("retiring", 7, 65536, move |_| body(owner, retire, freertos));
    if retire == Retire::DeleteFromCallback {
        unsafe { sim_ffi::sim_schedule_event(3, Some(delete_task)) };
    }
    if owner == Owner::Host {
        // The task's first slice, then host code masks.
        step(backend, &mut sim, 0);
        unsafe { sim_ffi::sim_enter_critical() };
    }
    if retire == Retire::DeleteByHost {
        // The task runs (and spins), then host code deletes it.
        step(backend, &mut sim, 1);
        unsafe { delete_task() };
    }
    run(backend, &mut sim, 5);
    unsafe { sim_ffi::sim_budget_set_limit(1_000_000) };
    match owner {
        Owner::Task => {
            assert!(
                !sim_ffi::is_critical_locked(),
                "{case}: the mask outlived its task"
            );
            assert_eq!(
                delivered(),
                [7],
                "{case}: IRQ 7 not delivered after release"
            );
        }
        Owner::Isr | Owner::Host => {
            let before: Vec<u32> = if owner == Owner::Isr { vec![6] } else { vec![] };
            assert!(
                sim_ffi::is_critical_locked(),
                "{case}: the {owner:?}'s mask did not survive"
            );
            assert_eq!(delivered(), before, "{case}: IRQ 7 ran before the unmask");
            // The owner unmasks.
            unsafe {
                if owner == Owner::Isr {
                    sim_enable_interrupts();
                } else {
                    sim_ffi::sim_exit_critical();
                }
            }
            run(backend, &mut sim, 10);
            let mut after = before;
            after.push(7);
            assert_eq!(delivered(), after, "{case}: IRQ 7 lost");
            assert!(!sim_ffi::is_critical_locked(), "{case}");
        }
    }
}

#[test]
fn a_retiring_task_releases_only_the_mask_it_owns() {
    let backends = [
        Backend::NativeStep,
        Backend::ZephyrStep,
        Backend::ZephyrLoop,
        Backend::FreeRtos,
        Backend::FreeRtosBounded,
    ];
    let mut failed = Vec::new();
    let mut ran = 0;
    for backend in backends {
        for owner in [Owner::Task, Owner::Isr, Owner::Host] {
            let retires: &[Retire] = if backend.freertos() {
                &[
                    Retire::Return,
                    Retire::Fault,
                    Retire::Exit,
                    Retire::SelfDelete,
                    Retire::DeleteFromCallback,
                    Retire::DeleteByHost,
                ]
            } else {
                &[Retire::Return, Retire::Fault, Retire::Exit]
            };
            for &retire in retires {
                // The Zephyr loop runs to the end in one call: host code
                // cannot mask between its steps.
                if backend == Backend::ZephyrLoop && owner == Owner::Host {
                    continue;
                }
                ran += 1;
                let result = std::thread::spawn(move || case(owner, retire, backend)).join();
                if result.is_err() {
                    failed.push(format!("{owner:?}/{retire:?}/{backend:?}"));
                }
            }
        }
    }
    assert!(ran > 0);
    assert!(failed.is_empty(), "failed: {failed:#?}");
}
