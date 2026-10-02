//! IRQ edges on the native and Zephyr schedulers (step and loop):
//!
//! - IRQ input that has arrived is taken before any task is resumed: a
//!   task that yielded continues only after the ISR.

use std::cell::{Cell, RefCell};
use std::sync::mpsc;
use std::time::Duration;

use sim_core::SimConfig;
use sim_ffi::device_ffi::sim_irq_set_handler;
use sim_ffi::simulator::Simulator;

#[derive(Clone, Copy, Debug)]
enum Backend {
    NativeStep,
    ZephyrStep,
    ZephyrLoop,
}

const ALL: [Backend; 3] = [
    Backend::NativeStep,
    Backend::ZephyrStep,
    Backend::ZephyrLoop,
];

thread_local! {
    static ORDER: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

fn note(what: &'static str) {
    ORDER.with(|o| o.borrow_mut().push(what));
}

unsafe extern "C" fn ordered_isr() {
    note("isr");
}

thread_local! {
    /// The Zephyr loop cannot be paused between steps: its thread stages
    /// the input itself before yielding.
    static STAGE_IN_THREAD: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "C" fn yielding_thread(
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
) {
    note("start");
    if STAGE_IN_THREAD.with(Cell::get) {
        sim_devices::irq::with_irq_mut(|c| c.raise_at(3, sim_ffi::sim_now_ticks()));
    }
    sim_ffi::sim_port_yield();
    note("continued");
}

/// Run `f` on its own thread (fresh thread-local state) with a deadline.
fn on_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(20)).ok()
}

fn step_until_done(backend: Backend) {
    match backend {
        Backend::ZephyrLoop => unsafe { sim_ffi::zephyr_ffi::sim_zephyr_start_scheduler() },
        _ => {
            for _ in 0..20 {
                let more = unsafe {
                    match backend {
                        Backend::ZephyrStep => sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick(),
                        _ => sim_ffi::sim_scheduler_tick(),
                    }
                };
                if more == 0 {
                    break;
                }
            }
        }
    }
}

/// A task yields; IRQ input arrives at the current tick (staged by host code
/// between steps); the task continues only after the ISR.
#[test]
fn arrived_irq_input_is_taken_before_the_task_continues() {
    for backend in ALL {
        let order = on_thread(move || {
            let mut sim = Simulator::new(SimConfig::default());
            sim.enable_owned_devices();
            let _active = sim.activate();
            unsafe { sim_irq_set_handler(3, Some(ordered_isr)) };
            match backend {
                Backend::NativeStep => {
                    sim_ffi::spawn_rust_task("interrupted", 1, 65536, |_| unsafe {
                        yielding_thread(
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                        )
                    });
                }
                _ => unsafe {
                    sim_ffi::zephyr_ffi::sim_zephyr_register_thread(
                        c"interrupted".as_ptr(),
                        Some(yielding_thread),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        65536,
                        1,
                    );
                },
            }
            if let Backend::ZephyrLoop = backend {
                STAGE_IN_THREAD.with(|s| s.set(true));
            } else {
                // One step: the task starts and yields.
                unsafe {
                    match backend {
                        Backend::ZephyrStep => sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick(),
                        _ => sim_ffi::sim_scheduler_tick(),
                    }
                };
                sim_devices::irq::with_irq_mut(|c| {
                    c.raise_at(3, unsafe { sim_ffi::sim_now_ticks() })
                });
            }
            step_until_done(backend);
            ORDER.with(|o| o.borrow().clone())
        });
        assert_eq!(
            order,
            Some(vec!["start", "isr", "continued"]),
            "{backend:?}"
        );
    }
}
