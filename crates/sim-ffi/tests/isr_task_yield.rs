//! Task-level yields and waits called from an ISR.
//!
//! An ISR runs on the fiber of the task it interrupted.  A yield it asks for
//! (`TaskContext::yield_now()`, like `sim_port_yield()` /
//! `portYIELD_FROM_ISR()`) waits for the ISR to return: the task it wakes
//! runs after the ISR's end, then the interrupted task continues.  That holds
//! on every scheduler: native, Zephyr step, Zephyr loop and FreeRTOS.
//!
//! A wait (a sleep, `sim_task_delay_until()`, a host I/O wait) cannot happen
//! in an ISR at all: it is firmware misuse, handled like a failed
//! `configASSERT()`.  On a task's fiber the interrupted task stops (the rest
//! of the ISR does not run, the machine's ISR state is released and later
//! IRQs are still taken); in scheduler context the process aborts with a
//! diagnostic (run in a child process).

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_long, c_ulong, c_void};

use sim_core::SimConfig;
use sim_ffi::device_ffi::{sim_irq_raise, sim_irq_set_handler};
use sim_ffi::simulator::Simulator;
use sim_ffi::TaskContext;

extern "C" {
    fn xTaskCreate(
        entry: unsafe extern "C" fn(*mut c_void),
        name: *const c_char,
        depth: u16,
        arg: *mut c_void,
        priority: c_ulong,
        handle: *mut *mut c_void,
    ) -> c_long;
    fn vTaskSuspend(task: *mut c_void);
    fn xTaskResumeFromISR(task: *mut c_void) -> c_long;
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Sched {
    Native,
    ZephyrStep,
    ZephyrLoop,
    FreeRtos,
}

const ALL: [Sched; 4] = [
    Sched::Native,
    Sched::ZephyrStep,
    Sched::ZephyrLoop,
    Sched::FreeRtos,
];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Wait {
    Sleep,
    DelayUntil,
    #[cfg(unix)]
    Io,
}

thread_local! {
    static ORDER: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    static SCHED: Cell<Sched> = const { Cell::new(Sched::Native) };
    static WAIT: Cell<Wait> = const { Cell::new(Wait::Sleep) };
    static WOKEN: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
}

fn note(s: &'static str) {
    ORDER.with(|o| o.borrow_mut().push(s));
}

fn current_ctx() -> TaskContext {
    TaskContext {
        task_id: sim_ffi::guest_runtime::active_task_id(),
    }
}

/// FreeRTOS's woken task: suspends itself until the ISR resumes it.
unsafe extern "C" fn freertos_woken(_: *mut c_void) {
    vTaskSuspend(std::ptr::null_mut());
    note("woken_task");
}

/// Wakes a higher-priority task, then yields with `TaskContext::yield_now()`.
unsafe extern "C" fn yielding_isr() {
    note("isr_start");
    if SCHED.with(Cell::get) == Sched::FreeRtos {
        xTaskResumeFromISR(WOKEN.with(Cell::get));
    } else {
        sim_ffi::spawn_rust_task("woken", 3, 65536, |_| note("woken_task"));
    }
    current_ctx().yield_now();
    note("isr_end");
}

/// Waits, which an ISR must never do.
unsafe extern "C" fn waiting_isr() {
    note("isr_start");
    match WAIT.with(Cell::get) {
        Wait::Sleep => current_ctx().sleep_for(1),
        Wait::DelayUntil => sim_ffi::sim_task_delay_until(sim_ffi::sim_now_ticks() + 1),
        #[cfg(unix)]
        Wait::Io => {
            use std::os::fd::AsRawFd;
            // A monitored descriptor that never becomes readable.
            let pair = Box::leak(Box::new(
                std::os::unix::net::UnixStream::pair().expect("socket pair"),
            ));
            let fd = pair.0.as_raw_fd();
            assert_eq!(sim_ffi::net_ffi::sim_host_register_fd(fd), 0);
            sim_ffi::net_ffi::sim_host_block_on_fd(fd)
        }
    }
    note("isr_end");
}

unsafe extern "C" fn later_isr() {
    note("later_isr");
}

/// A machine for `sched`, with the woken task in place on FreeRTOS.
fn machine(sched: Sched) -> Simulator {
    ORDER.with(|o| o.borrow_mut().clear());
    SCHED.with(|s| s.set(sched));
    let mut sim = Simulator::new(SimConfig::default());
    sim.enable_owned_devices();
    sim
}

fn run_to_end(sim: &mut Simulator, sched: Sched) {
    match sched {
        Sched::ZephyrLoop => unsafe { sim_ffi::zephyr_ffi::sim_zephyr_start_scheduler() },
        _ => {
            sim.set_scheduler_limit(Some(50));
            for _ in 0..50 {
                let more = unsafe {
                    if sched == Sched::ZephyrStep {
                        sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick()
                    } else {
                        sim_ffi::sim_scheduler_tick()
                    }
                };
                if more == 0 {
                    break;
                }
            }
        }
    }
}

fn yield_order(sched: Sched) -> Vec<&'static str> {
    let mut sim = machine(sched);
    let _active = sim.activate();
    unsafe { sim_irq_set_handler(6, Some(yielding_isr)) };
    if sched == Sched::FreeRtos {
        let mut handle = std::ptr::null_mut();
        let created = unsafe {
            xTaskCreate(
                freertos_woken,
                c"woken".as_ptr(),
                256,
                std::ptr::null_mut(),
                3,
                &mut handle,
            )
        };
        assert_eq!(created, 1);
        WOKEN.with(|w| w.set(handle));
    }
    sim_ffi::spawn_rust_task("interrupted", 1, 65536, |_| unsafe {
        sim_irq_raise(6);
        note("interrupted_continued");
    });
    run_to_end(&mut sim, sched);
    ORDER.with(|o| o.borrow().clone())
}

#[test]
fn a_task_context_yield_in_an_isr_waits_for_the_isr_to_return() {
    for sched in ALL {
        assert_eq!(
            yield_order(sched),
            [
                "isr_start",
                "isr_end",
                "woken_task",
                "interrupted_continued"
            ],
            "{sched:?}"
        );
    }
}

fn wait_order(sched: Sched, wait: Wait) -> Vec<&'static str> {
    WAIT.with(|w| w.set(wait));
    let mut sim = machine(sched);
    let _active = sim.activate();
    unsafe {
        sim_irq_set_handler(6, Some(waiting_isr));
        sim_irq_set_handler(7, Some(later_isr));
    }
    if sched == Sched::FreeRtos {
        // Makes it a FreeRTOS machine; never runs before the others end.
        unsafe extern "C" fn background(_: *mut c_void) {}
        let created = unsafe {
            xTaskCreate(
                background,
                c"background".as_ptr(),
                256,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(created, 1);
    }
    sim_ffi::spawn_rust_task("interrupted", 5, 65536, |_| unsafe {
        sim_irq_raise(6);
        note("interrupted_continued");
    });
    sim_ffi::spawn_rust_task("peer", 4, 65536, |_| unsafe {
        note("peer");
        sim_irq_raise(7);
    });
    run_to_end(&mut sim, sched);
    assert!(!sim_ffi::device_ffi::in_isr(), "{sched:?} {wait:?}");
    assert!(!sim_ffi::is_critical_locked(), "{sched:?} {wait:?}");
    ORDER.with(|o| o.borrow().clone())
}

#[test]
fn a_wait_in_an_isr_on_a_task_fiber_stops_that_task() {
    let waits = [
        Wait::Sleep,
        Wait::DelayUntil,
        #[cfg(unix)]
        Wait::Io,
    ];
    for sched in ALL {
        for wait in waits {
            // The interrupted task and the rest of its ISR never ran on;
            // its peer did, and so did a later ISR.
            assert_eq!(
                wait_order(sched, wait),
                ["isr_start", "peer", "later_isr"],
                "{sched:?} {wait:?}"
            );
        }
    }
}

/// The child process's part: an IRQ taken while no task runs.
fn scheduler_context_wait() {
    let mut sim = machine(Sched::Native);
    let _active = sim.activate();
    WAIT.with(|w| w.set(Wait::Sleep));
    unsafe {
        sim_irq_set_handler(6, Some(waiting_isr));
        sim_irq_raise(6);
    }
    println!("CHILD: the ISR returned past its wait");
}

#[test]
fn a_wait_in_a_scheduler_context_isr_aborts_with_a_diagnostic() {
    if std::env::var_os("COSTAR_ISR_WAIT_CHILD").is_some() {
        scheduler_context_wait();
        return;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "a_wait_in_a_scheduler_context_isr_aborts_with_a_diagnostic",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("COSTAR_ISR_WAIT_CHILD", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(out.status.signal(), Some(6), "{:?}\n{stderr}", out.status);
    }
    assert!(!out.status.success());
    assert!(
        stderr.contains("called from an ISR") && stderr.contains("scheduler context"),
        "no diagnostic: {stderr}"
    );
    assert!(!stdout.contains("returned past"), "{stdout}");
}

thread_local! {
    static PORT_YIELDS: Cell<u32> = const { Cell::new(0) };
}

unsafe extern "C" fn port_yielding_isr() {
    PORT_YIELDS.with(|y| y.set(y.get() + 1));
    sim_ffi::sim_port_yield();
}

/// `portYIELD_FROM_ISR()` (`sim_port_yield()`) from an ISR the engine runs
/// in scheduler context, with no task fiber, is valid on every scheduler:
/// no fault is recorded.
#[test]
fn a_port_yield_from_a_scheduler_context_isr_is_no_fault() {
    for sched in [Sched::Native, Sched::ZephyrStep, Sched::FreeRtos] {
        PORT_YIELDS.with(|y| y.set(0));
        let mut sim = machine(sched);
        let global = sim.sim_global.clone();
        let _active = sim.activate();
        unsafe { sim_irq_set_handler(6, Some(port_yielding_isr)) };
        if sched == Sched::FreeRtos {
            unsafe extern "C" fn background(_: *mut c_void) {}
            let created = unsafe {
                xTaskCreate(
                    background,
                    c"background".as_ptr(),
                    256,
                    std::ptr::null_mut(),
                    1,
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(created, 1);
        }
        // Delivered by the scheduler before any task runs, outside a fiber.
        sim_devices::irq::with_irq_mut(|c| c.raise_at(6, 0));
        run_to_end(&mut sim, sched);
        sim_ffi::flush_trace();
        assert_eq!(
            PORT_YIELDS.with(Cell::get),
            1,
            "{sched:?}: the ISR did not run"
        );
        let fatal = global
            .borrow()
            .trace
            .as_ref()
            .unwrap()
            .events
            .iter()
            .any(|e| matches!(e, sim_core::TraceEvent::Fatal { .. }));
        assert!(!fatal, "{sched:?}: a fault was recorded");
    }
}
