//! Waits must survive arbitrary resumes, and their wake-ups must not be
//! lost.
//!
//! A task waiting in `sim_task_delay_until()` or `sim_host_block_on_fd()`
//! can be resumed by its wait's own wake-up, by FreeRTOS adopting it, or
//! by the firmware (`vTaskSuspend()` + `vTaskResume()` of its TCB).  Only
//! the wait's condition may end it.  Descriptor readiness may arrive at any
//! point: before the wait, while the task waits natively or in the kernel,
//! after a spurious resume, or together with one.
#![cfg(unix)]

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;
use std::ffi::{c_char, c_long, c_ulong, c_void};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
extern "C" {
    fn xTaskCreate(
        entry: unsafe extern "C" fn(*mut c_void),
        name: *const c_char,
        depth: u16,
        arg: *mut c_void,
        priority: c_ulong,
        handle: *mut *mut c_void,
    ) -> c_long;
    fn xTaskGetCurrentTaskHandle() -> *mut c_void;
    fn vTaskSuspend(t: *mut c_void);
    fn vTaskResume(t: *mut c_void);
    fn vTaskDelay(ticks: u32);
    fn costar_test_abi_delay_boot();
}
#[test]
fn native_fd_readiness_must_end_wait() {
    use std::{
        io::Write,
        os::{fd::AsRawFd, unix::net::UnixStream},
    };
    let (reader, mut writer) = UnixStream::pair().unwrap();
    let fd = reader.as_raw_fd();
    let mut sim = Simulator::new(SimConfig::default());
    let done = Arc::new(AtomicBool::new(false));
    let copy = done.clone();
    let _a = sim.activate();
    assert_eq!(unsafe { sim_ffi::net_ffi::sim_host_register_fd(fd) }, 0);
    sim_ffi::spawn_rust_task("reader", 3, 65536, move |_| {
        unsafe { sim_ffi::net_ffi::sim_host_block_on_fd(fd) };
        copy.store(true, Ordering::SeqCst);
    });
    unsafe { sim_ffi::sim_scheduler_tick() };
    writer.write_all(b"x").unwrap();
    for _ in 0..5 {
        unsafe { sim_ffi::sim_scheduler_tick() };
    }
    sim_ffi::net_ffi::sim_host_deregister_fd(fd);
    assert!(
        done.load(Ordering::SeqCst),
        "readable FD did not finish native wait; state={:?}",
        sim.sim_global.borrow().tasks[0].state
    );
}
struct WaitData {
    handle: *mut c_void,
    fd: i32,
    done: bool,
}
unsafe extern "C" fn fd_waiter(arg: *mut c_void) {
    let d = arg as *mut WaitData;
    (*d).handle = xTaskGetCurrentTaskHandle();
    sim_ffi::net_ffi::sim_host_block_on_fd((*d).fd);
    (*d).done = true;
}
unsafe extern "C" fn resume_waiter(arg: *mut c_void) {
    let d = arg as *mut WaitData;
    vTaskResume((*d).handle);
}
#[test]
fn resumed_fd_waiter_must_keep_waiting_without_data() {
    use std::os::{fd::AsRawFd, unix::net::UnixStream};
    let (reader, _writer) = UnixStream::pair().unwrap();
    let mut d = WaitData {
        handle: std::ptr::null_mut(),
        fd: reader.as_raw_fd(),
        done: false,
    };
    let mut sim = Simulator::new(SimConfig::default());
    let _a = sim.activate();
    assert_eq!(unsafe { sim_ffi::net_ffi::sim_host_register_fd(d.fd) }, 0);
    unsafe {
        let p = &mut d as *mut _ as *mut c_void;
        assert_eq!(
            xTaskCreate(
                fd_waiter,
                c"waiter".as_ptr(),
                128,
                p,
                5,
                std::ptr::null_mut()
            ),
            1
        );
        assert_eq!(
            xTaskCreate(
                resume_waiter,
                c"resumer".as_ptr(),
                128,
                p,
                3,
                std::ptr::null_mut()
            ),
            1
        );
    }
    sim.sim_global.borrow_mut().scheduler_limit = Some(0);
    unsafe { sim_ffi::sim_scheduler_tick() };
    sim_ffi::net_ffi::sim_host_deregister_fd(d.fd);
    assert!(!d.done, "I/O wait returned with no data after vTaskResume");
}
struct SleepData {
    handle: *mut c_void,
    woke: u64,
}
unsafe extern "C" fn sleeper(arg: *mut c_void) {
    let d = arg as *mut SleepData;
    (*d).handle = xTaskGetCurrentTaskHandle();
    sim_ffi::sim_task_delay_until(10);
    (*d).woke = sim_ffi::sim_now_ticks();
}
unsafe extern "C" fn suspend_resume_sleeper(arg: *mut c_void) {
    let d = arg as *mut SleepData;
    vTaskSuspend((*d).handle);
    vTaskResume((*d).handle);
}
#[test]
fn resumed_abi_sleeper_must_keep_deadline() {
    let mut d = SleepData {
        handle: std::ptr::null_mut(),
        woke: u64::MAX,
    };
    let mut sim = Simulator::new(SimConfig::default());
    let _a = sim.activate();
    unsafe {
        let p = &mut d as *mut _ as *mut c_void;
        assert_eq!(
            xTaskCreate(
                sleeper,
                c"sleeper".as_ptr(),
                128,
                p,
                5,
                std::ptr::null_mut()
            ),
            1
        );
        assert_eq!(
            xTaskCreate(
                suspend_resume_sleeper,
                c"resumer".as_ptr(),
                128,
                p,
                3,
                std::ptr::null_mut()
            ),
            1
        );
    }
    sim.sim_global.borrow_mut().scheduler_limit = Some(0);
    unsafe { sim_ffi::sim_scheduler_tick() };
    assert_eq!(
        d.woke,
        u64::MAX,
        "ABI sleep returned before its tick-10 deadline"
    );
}

// ── Adversarial cases ─────────────────────────────────────────────────

use std::io::{Read, Write};
use std::os::{fd::AsRawFd, unix::net::UnixStream};
use std::sync::atomic::{AtomicU32, AtomicUsize};

/// Step `sim` `n` times, bounded (World-style, one tick per step) or not.
fn step(g: &std::rc::Rc<std::cell::RefCell<sim_ffi::SimGlobal>>, world: bool, n: u64) {
    for _ in 0..n {
        if world {
            let now = g.borrow().scheduler_sim_time;
            g.borrow_mut().scheduler_limit = Some(now + 1);
        } else {
            g.borrow_mut().scheduler_limit = None;
        }
        unsafe { sim_ffi::sim_scheduler_tick() };
    }
}

struct Spurious {
    handle: AtomicUsize,
    woke: AtomicU32,
}

unsafe extern "C" fn sleeper_until_10(arg: *mut c_void) {
    let d = &*(arg as *const Spurious);
    d.handle
        .store(xTaskGetCurrentTaskHandle() as usize, Ordering::SeqCst);
    sim_ffi::sim_task_delay_until(10);
    d.woke
        .store(sim_ffi::sim_now_ticks() as u32, Ordering::SeqCst);
}

/// Suspends and resumes the other task's TCB at every tick 0..9.
unsafe extern "C" fn resume_every_tick(arg: *mut c_void) {
    let d = &*(arg as *const Spurious);
    for _ in 0..10 {
        let h = d.handle.load(Ordering::SeqCst) as *mut c_void;
        if !h.is_null() {
            vTaskSuspend(h);
            vTaskResume(h);
        }
        vTaskDelay(1);
    }
}

#[test]
fn delay_keeps_its_deadline_through_repeated_spurious_resumes() {
    for world in [false, true] {
        let d = Box::leak(Box::new(Spurious {
            handle: AtomicUsize::new(0),
            woke: AtomicU32::new(u32::MAX),
        }));
        let p = d as *mut Spurious as *mut c_void;
        let mut sim = Simulator::new(SimConfig::default());
        let g = sim.sim_global.clone();
        let _a = sim.activate();
        unsafe {
            xTaskCreate(
                sleeper_until_10,
                c"sleeper".as_ptr(),
                128,
                p,
                5,
                std::ptr::null_mut(),
            );
            xTaskCreate(
                resume_every_tick,
                c"resumer".as_ptr(),
                128,
                p,
                3,
                std::ptr::null_mut(),
            );
        }
        step(&g, world, if world { 30 } else { 300 });
        assert_eq!(d.woke.load(Ordering::SeqCst), 10, "world={world}");
    }
}

#[test]
fn native_sleep_keeps_its_deadline_through_spurious_resumes() {
    for world in [false, true] {
        let d = Box::leak(Box::new(Spurious {
            handle: AtomicUsize::new(0),
            woke: AtomicU32::new(u32::MAX),
        }));
        let p = d as *mut Spurious as *mut c_void;
        let dd: &'static Spurious = d;
        let mut sim = Simulator::new(SimConfig::default());
        let g = sim.sim_global.clone();
        let _a = sim.activate();
        unsafe { costar_test_abi_delay_boot() };
        sim_ffi::spawn_rust_task("native", 5, 4096, move |ctx| {
            dd.handle.store(
                unsafe { xTaskGetCurrentTaskHandle() } as usize,
                Ordering::SeqCst,
            );
            ctx.sleep_until(10);
            dd.woke.store(ctx.now() as u32, Ordering::SeqCst);
        });
        unsafe {
            xTaskCreate(
                resume_every_tick,
                c"resumer".as_ptr(),
                128,
                p,
                4,
                std::ptr::null_mut(),
            );
        }
        step(&g, world, if world { 30 } else { 300 });
        assert_eq!(d.woke.load(Ordering::SeqCst), 10, "world={world}");
    }
}

/// When the data is written relative to the wait.
#[derive(Debug, Clone, Copy)]
enum Arrival {
    /// Before the task starts waiting: the descriptor is already readable.
    BeforeWait,
    /// While the task waits (after a first step).
    WhileWaiting,
    /// After a spurious resume of the waiting task's TCB.
    AfterSpuriousResume,
    /// In the same step as a spurious resume.
    WithSpuriousResume,
}

struct IoCase {
    handle: AtomicUsize,
    done: AtomicU32,
    resume_now: AtomicBool,
}

/// Resumes the I/O waiter's TCB whenever asked to.
unsafe extern "C" fn resume_on_request(arg: *mut c_void) {
    let d = &*(arg as *const IoCase);
    for _ in 0..40 {
        if d.resume_now.swap(false, Ordering::SeqCst) {
            let h = d.handle.load(Ordering::SeqCst) as *mut c_void;
            if !h.is_null() {
                vTaskResume(h);
            }
        }
        vTaskDelay(1);
    }
}

/// A task on a FreeRTOS machine (`freertos`) or a native-only machine waits
/// on a socket twice; data for each wait arrives as `arrival` says.  Each
/// wait must end exactly when its data is there: neither earlier (spurious
/// resume, stale readiness from the first wait) nor never.
fn io_case(freertos: bool, world: bool, arrival: Arrival) {
    let case = format!("freertos={freertos} world={world} arrival={arrival:?}");
    let (reader, mut writer) = UnixStream::pair().unwrap();
    reader.set_nonblocking(true).unwrap();
    let fd = reader.as_raw_fd();
    let d: &'static IoCase = Box::leak(Box::new(IoCase {
        handle: AtomicUsize::new(0),
        done: AtomicU32::new(0),
        resume_now: AtomicBool::new(false),
    }));
    let mut sim = Simulator::new(SimConfig::default());
    let g = sim.sim_global.clone();
    let _a = sim.activate();
    assert_eq!(unsafe { sim_ffi::net_ffi::sim_host_register_fd(fd) }, 0);
    if freertos {
        unsafe {
            costar_test_abi_delay_boot();
            xTaskCreate(
                resume_on_request,
                c"resumer".as_ptr(),
                128,
                d as *const IoCase as *mut c_void,
                4,
                std::ptr::null_mut(),
            );
        }
    }
    if matches!(arrival, Arrival::BeforeWait) {
        writer.write_all(b"1").unwrap();
    }
    let mut reader = reader;
    sim_ffi::spawn_rust_task("io", 5, 65536, move |_| {
        if freertos {
            d.handle.store(
                unsafe { xTaskGetCurrentTaskHandle() } as usize,
                Ordering::SeqCst,
            );
        }
        let mut buf = [0u8; 8];
        for round in 1..=2 {
            unsafe { sim_ffi::net_ffi::sim_host_block_on_fd(fd) };
            let n = reader.read(&mut buf).unwrap_or(0);
            assert!(n > 0, "wait {round} ended without data");
            d.done.store(round, Ordering::SeqCst);
        }
    });
    let spurious = |d: &IoCase| d.resume_now.store(true, Ordering::SeqCst);

    for round in 1..=2u32 {
        step(&g, world, 2);
        if round == 2 || !matches!(arrival, Arrival::BeforeWait) {
            assert_eq!(
                d.done.load(Ordering::SeqCst),
                round - 1,
                "{case}: wait {round} ended early"
            );
        }
        match arrival {
            Arrival::BeforeWait if round == 1 => {}
            Arrival::BeforeWait | Arrival::WhileWaiting => writer.write_all(b"x").unwrap(),
            Arrival::AfterSpuriousResume => {
                if freertos {
                    spurious(d);
                    step(&g, world, 3);
                    assert_eq!(
                        d.done.load(Ordering::SeqCst),
                        round - 1,
                        "{case}: spurious resume ended wait {round}"
                    );
                }
                writer.write_all(b"x").unwrap();
            }
            Arrival::WithSpuriousResume => {
                if freertos {
                    spurious(d);
                }
                writer.write_all(b"x").unwrap();
            }
        }
        for _ in 0..10 {
            step(&g, world, 1);
            if d.done.load(Ordering::SeqCst) >= round {
                break;
            }
        }
        assert_eq!(
            d.done.load(Ordering::SeqCst),
            round,
            "{case}: wait {round} never ended"
        );
    }
    sim_ffi::net_ffi::sim_host_deregister_fd(fd);
}

#[test]
fn io_waits_end_exactly_when_data_arrives() {
    for freertos in [false, true] {
        for world in [false, true] {
            for arrival in [
                Arrival::BeforeWait,
                Arrival::WhileWaiting,
                Arrival::AfterSpuriousResume,
                Arrival::WithSpuriousResume,
            ] {
                io_case(freertos, world, arrival);
            }
        }
    }
}
