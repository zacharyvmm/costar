//! Callbacks, timers and a task that dispatches callbacks itself, on every
//! backend (native step, Zephyr step, Zephyr loop, FreeRTOS).
//!
//! - A task dispatching the callbacks due now is never suspended in the
//!   middle of its dispatch: a switch an ISR (raised by a callback) asks
//!   for waits until the dispatch ends, so the dispatch completes, and the
//!   scheduler never spins on callbacks a suspended dispatch would own.
//! - After every task slice the work due now runs in order: a callback due
//!   now (which disarms a timer) runs before the timer's expiry is taken.

use std::cell::RefCell;
use std::sync::mpsc;
use std::time::Duration;

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;

extern "C" {
    fn costar_test_spawn_task(name: *const std::ffi::c_char, body: extern "C" fn(), priority: u32);
}

thread_local! {
    static ORDER: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

fn note(s: &'static str) {
    ORDER.with(|o| o.borrow_mut().push(s));
}

fn order() -> Vec<&'static str> {
    ORDER.with(|o| o.borrow().clone())
}

#[derive(Clone, Copy, Debug)]
enum Backend {
    NativeStep,
    ZephyrStep,
    ZephyrLoop,
    FreeRtos,
}

const BACKENDS: [Backend; 4] = [
    Backend::NativeStep,
    Backend::ZephyrStep,
    Backend::ZephyrLoop,
    Backend::FreeRtos,
];

extern "C" fn anchor() {}

fn boot(backend: Backend, sim: &mut Simulator) {
    if let Backend::FreeRtos = backend {
        unsafe { costar_test_spawn_task(c"anchor".as_ptr(), anchor, 1) };
        sim.set_scheduler_limit(Some(0));
    }
}

fn run(backend: Backend, steps: usize) {
    match backend {
        Backend::ZephyrLoop => unsafe { sim_ffi::zephyr_ffi::sim_zephyr_start_scheduler() },
        Backend::ZephyrStep => {
            for _ in 0..steps {
                unsafe { sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick() };
            }
        }
        _ => {
            for _ in 0..steps {
                unsafe { sim_ffi::sim_scheduler_tick() };
            }
        }
    }
}

/// Runs `case` on a thread of its own; `None` if it hung.
fn on_own_thread<T: Send + 'static>(case: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(case());
    });
    rx.recv_timeout(Duration::from_secs(20)).ok()
}

unsafe extern "C" fn isr_wakes_high() {
    note("isr");
    sim_ffi::spawn_rust_task("high", 3, 65536, |_| note("high"));
    sim_ffi::sim_port_yield();
}

unsafe extern "C" fn first() {
    note("callback start");
    sim_ffi::device_ffi::sim_irq_raise(6);
    note("callback end");
}

unsafe extern "C" fn second() {
    note("second");
}

#[test]
fn a_task_dispatching_callbacks_is_not_switched_away_mid_dispatch() {
    let mut failed = Vec::new();
    for backend in BACKENDS {
        let result = on_own_thread(move || {
            let mut sim = Simulator::new(SimConfig::default());
            sim.enable_owned_devices();
            let _active = sim.activate();
            unsafe { sim_ffi::device_ffi::sim_irq_set_handler(6, Some(isr_wakes_high)) };
            boot(backend, &mut sim);
            sim_ffi::spawn_rust_task("low", 1, 65536, |_| {
                unsafe {
                    sim_ffi::sim_schedule_event(0, Some(first));
                    sim_ffi::sim_schedule_event(0, Some(second));
                }
                sim_ffi::dispatch_events(0);
                note("low continued");
            });
            run(backend, 10);
            order()
        });
        let Some(order) = result else {
            failed.push(format!("{backend:?}: hung"));
            continue;
        };
        let at = |s| order.iter().position(|&o| o == s);
        let ok = at("callback end").is_some()
            && at("second").is_some()
            && at("high").is_some()
            && at("low continued").is_some()
            && at("second") < at("high");
        if !ok {
            failed.push(format!("{backend:?}: {order:?}"));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

unsafe extern "C" fn timer_isr() {
    note("timer isr");
}

unsafe extern "C" fn disarm() {
    note("disarm callback");
    sim_ffi::device_ffi::sim_timer_disarm(0);
}

#[test]
fn a_callback_due_after_a_slice_runs_before_the_timer() {
    let mut failed = Vec::new();
    for backend in BACKENDS {
        let result = on_own_thread(move || {
            let mut sim = Simulator::new(SimConfig::default());
            sim.enable_owned_devices();
            let _active = sim.activate();
            sim_devices::timer_insert(sim_devices::VirtualTimer::new_oneshot(0, 6));
            unsafe { sim_ffi::device_ffi::sim_irq_set_handler(6, Some(timer_isr)) };
            boot(backend, &mut sim);
            sim.set_scheduler_limit(Some(0));
            sim_ffi::spawn_rust_task("task", 1, 65536, |ctx| {
                unsafe {
                    sim_ffi::sim_schedule_event(0, Some(disarm));
                    sim_ffi::device_ffi::sim_timer_arm(0, 0);
                }
                ctx.yield_now();
            });
            run(backend, 5);
            order()
        });
        match result {
            Some(order) if order == ["disarm callback"] => {}
            other => failed.push(format!("{backend:?}: {other:?}")),
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
