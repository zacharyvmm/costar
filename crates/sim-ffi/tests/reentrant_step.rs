//! A peripheral callback that calls `sim_scheduler_tick()` (a scheduler
//! step from inside the machine's own step) is tolerated misuse: the nested
//! call is a no-op that returns 1 ("busy, try again"), and the running step
//! goes on.  Two finite callbacks due at one tick both run, and no false
//! interrupt storm is declared (FreeRTOS bounded and standalone, native
//! step, Zephyr step).

use std::cell::{Cell, RefCell};

use sim_core::{SimConfig, TraceEvent};
use sim_ffi::simulator::Simulator;

extern "C" {
    fn costar_test_spawn_task(name: *const std::ffi::c_char, body: extern "C" fn(), priority: u32);
}

thread_local! {
    static ORDER: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    static NESTED: Cell<Option<u32>> = const { Cell::new(None) };
    static ZEPHYR: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "C" fn steps_the_scheduler() {
    ORDER.with(|o| o.borrow_mut().push("first"));
    let more = if ZEPHYR.with(Cell::get) {
        sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick()
    } else {
        sim_ffi::sim_scheduler_tick()
    };
    NESTED.with(|n| n.set(Some(more)));
    // Starting the scheduler from here is a no-op too.
    sim_ffi::sim_start_scheduler();
}

unsafe extern "C" fn second() {
    ORDER.with(|o| o.borrow_mut().push("second"));
}

extern "C" fn anchor() {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    FreeRtosBounded,
    FreeRtos,
    NativeStep,
    ZephyrStep,
}

fn case(backend: Backend) {
    ORDER.with(|o| o.borrow_mut().clear());
    NESTED.with(|n| n.set(None));
    ZEPHYR.with(|z| z.set(backend == Backend::ZephyrStep));
    let mut sim = Simulator::new(SimConfig::default());
    let g = sim.sim_global.clone();
    let _active = sim.activate();
    if matches!(backend, Backend::FreeRtos | Backend::FreeRtosBounded) {
        unsafe { costar_test_spawn_task(c"anchor".as_ptr(), anchor, 1) };
    }
    if backend == Backend::FreeRtosBounded {
        sim.set_scheduler_limit(Some(0));
        // Park the machine at tick 0.
        unsafe { sim_ffi::sim_scheduler_tick() };
    }
    unsafe {
        sim_ffi::sim_schedule_event(0, Some(steps_the_scheduler));
        sim_ffi::sim_schedule_event(0, Some(second));
    }
    for _ in 0..10 {
        unsafe {
            if backend == Backend::ZephyrStep {
                sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick();
            } else {
                sim_ffi::sim_scheduler_tick();
            }
        }
    }
    assert_eq!(
        ORDER.with(|o| o.borrow().clone()),
        ["first", "second"],
        "{backend:?}"
    );
    assert_eq!(
        NESTED.with(Cell::get),
        Some(1),
        "{backend:?}: nested step result"
    );
    assert!(!sim_ffi::freertos::halted(), "{backend:?}: false storm");
    sim_ffi::flush_trace();
    let storms = g
        .borrow()
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                TraceEvent::UserU32 {
                    label: "irq_storm",
                    ..
                }
            )
        })
        .count();
    assert_eq!(storms, 0, "{backend:?}");
}

#[test]
fn a_callback_stepping_the_scheduler_is_a_no_op() {
    for backend in [
        Backend::FreeRtosBounded,
        Backend::FreeRtos,
        Backend::NativeStep,
        Backend::ZephyrStep,
    ] {
        case(backend);
    }
}
