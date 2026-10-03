//! A task that deletes itself and keeps running for a while.
//!
//! `vTaskDelete(NULL)` puts the task's TCB on FreeRTOS's termination list at
//! once, for the idle task to free; the switch away from it is pended while
//! the task is inside a critical section (and `configASSERT()` stops a task
//! that deletes itself holding the scheduler lock).  Whatever the task does
//! next — it faults, returns, yields and waits, or uses up its CPU budget —
//! no engine path may touch the deleted TCB again: suspending it took it
//! off the termination list behind the kernel's back, and deleting it again
//! counted it twice; either way the idle task's cleanup crashed.
//!
//! The matrix: {native Rust task, firmware task (`xTaskCreate()`), legacy
//! `sim_create_task()` task} × {self-delete in a critical section, holding
//! the scheduler lock, plain} × {then fault, return, yield then sleep, budget
//! preemption, unmask, host I/O wait} × {bounded, unbounded stepping}.  In every
//! case the kernel's count of deleted tasks waiting for cleanup matches the
//! termination list after every step, the idle task frees every deleted TCB,
//! the machine is left unmasked, and a healthy peer runs to completion.

use std::ffi::{c_char, c_long, c_ulong, c_void};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use sim_core::SimConfig;
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
    fn vTaskDelete(task: *mut c_void);
    fn vTaskDelay(ticks: u32);
    fn vTaskSuspendAll();
    fn xTaskResumeAll() -> c_long;
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    /// `spawn_rust_task()`, adopted by FreeRTOS.
    Native,
    /// `xTaskCreate()`: returning goes through the port's task-return path.
    Firmware,
    /// `sim_create_task()` alone, adopted by FreeRTOS.
    Legacy,
}

#[derive(Clone, Copy, Debug)]
enum Hold {
    /// `vTaskDelete(NULL)` inside a critical section: the switch is pended.
    Critical,
    /// `vTaskDelete(NULL)` holding the scheduler lock: `configASSERT()`.
    SchedulerLock,
    /// `vTaskDelete(NULL)` unmasked: the task never runs again.
    Plain,
}

#[derive(Clone, Copy, Debug)]
enum Then {
    /// A Rust panic (native) or a failed `configASSERT()` (C entry points).
    Fault,
    Return,
    /// `taskYIELD()` (pended while masked), then a delay.
    YieldSleep,
    /// Busy until the CPU budget runs out.
    Budget,
    /// Leaves the critical section (or releases the lock): the pended
    /// switch happens.
    Unmask,
    /// `sim_host_block_on_fd()` on a registered descriptor.
    #[cfg(unix)]
    IoWait,
}

#[derive(Clone, Copy, Debug)]
struct Case {
    kind: Kind,
    hold: Hold,
    then: Then,
    bounded: bool,
}

/// What the deleting task got to do.
struct Progress {
    /// It ran on after `vTaskDelete(NULL)` returned.
    after_delete: AtomicBool,
    /// It ran on after what it did next (other than returning or
    /// faulting): a deleted task's wait, preemption or unmask must end it.
    after_then: AtomicBool,
}

struct Deleter {
    case: Case,
    progress: &'static Progress,
}

/// The deleting task's body, shared by every kind of task.
unsafe fn delete_self_then(d: &Deleter, ctx: Option<&TaskContext>) {
    match d.case.hold {
        Hold::Critical => sim_ffi::sim_enter_critical(),
        Hold::SchedulerLock => vTaskSuspendAll(),
        Hold::Plain => {}
    }
    vTaskDelete(std::ptr::null_mut());
    d.progress.after_delete.store(true, Ordering::SeqCst);
    match d.case.then {
        Then::Fault => match ctx {
            Some(_) => panic!("fault after deleting itself"),
            None => sim_ffi::freertos::sim_assert_failed(c"deleted_task".as_ptr(), 1),
        },
        Then::Return => return,
        Then::YieldSleep => match ctx {
            Some(ctx) => {
                ctx.yield_now();
                ctx.sleep_for(1);
            }
            None => {
                sim_ffi::sim_port_yield();
                sim_ffi::sim_task_delay_until(sim_ffi::sim_now_ticks() + 1);
            }
        },
        Then::Budget => {
            sim_ffi::sim_budget_set_limit(1);
            for _ in 0..1000 {
                sim_ffi::sim_budget_poll(std::ptr::null(), 0);
            }
        }
        Then::Unmask => match d.case.hold {
            Hold::Critical => sim_ffi::sim_exit_critical(),
            Hold::SchedulerLock => {
                xTaskResumeAll();
            }
            Hold::Plain => {}
        },
        #[cfg(unix)]
        Then::IoWait => {
            use std::os::fd::AsRawFd;
            // Never readable: nothing is ever written to the other end.
            let pair = Box::leak(Box::new(
                std::os::unix::net::UnixStream::pair().expect("socket pair"),
            ));
            let fd = pair.0.as_raw_fd();
            assert_eq!(sim_ffi::net_ffi::sim_host_register_fd(fd), 0);
            sim_ffi::net_ffi::sim_host_block_on_fd(fd);
        }
    }
    d.progress.after_then.store(true, Ordering::SeqCst);
}

unsafe extern "C" fn c_deleter(arg: *mut c_void) {
    delete_self_then(&*(arg as *const Deleter), None);
}

/// Wakes up five times, one tick apart, then returns.
unsafe extern "C" fn peer(arg: *mut c_void) {
    let rounds = &*(arg as *const AtomicU32);
    for _ in 0..5 {
        rounds.fetch_add(1, Ordering::SeqCst);
        vTaskDelay(1);
    }
}

fn check_bookkeeping(case: Case, when: &str) {
    let (pending, listed) = sim_ffi::freertos::termination_bookkeeping();
    assert_eq!(
        pending, listed,
        "{case:?}: {when}: {pending} deleted tasks counted for cleanup, {listed} listed"
    );
}

fn run(case: Case) {
    let mut sim = Simulator::new(SimConfig::default());
    let _active = sim.activate();

    // The peer, a firmware task, makes this a FreeRTOS machine.
    let rounds: &'static AtomicU32 = Box::leak(Box::new(AtomicU32::new(0)));
    let created = unsafe {
        xTaskCreate(
            peer,
            c"peer".as_ptr(),
            256,
            rounds as *const AtomicU32 as *mut c_void,
            6,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(created, 1);

    let progress: &'static Progress = Box::leak(Box::new(Progress {
        after_delete: AtomicBool::new(false),
        after_then: AtomicBool::new(false),
    }));
    let deleter: &'static Deleter = Box::leak(Box::new(Deleter { case, progress }));
    let arg = deleter as *const Deleter as *mut c_void;
    match case.kind {
        Kind::Native => {
            sim_ffi::spawn_rust_task("deleter", 7, 65536, move |ctx| unsafe {
                delete_self_then(deleter, Some(&ctx));
            });
        }
        Kind::Firmware => {
            let created = unsafe {
                xTaskCreate(
                    c_deleter,
                    c"deleter".as_ptr(),
                    256,
                    arg,
                    7,
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(created, 1);
        }
        Kind::Legacy => unsafe {
            sim_ffi::sim_create_task(c"deleter".as_ptr(), Some(c_deleter), arg, 256, 7);
        },
    }

    sim.set_scheduler_limit(case.bounded.then_some(20));
    for step in 0..60 {
        let more = unsafe { sim_ffi::sim_scheduler_tick() };
        check_bookkeeping(case, &format!("after step {step}"));
        if more == 0 && !case.bounded {
            break;
        }
    }

    assert_eq!(
        progress.after_delete.load(Ordering::SeqCst),
        matches!(case.hold, Hold::Critical),
        "{case:?}: only a task masked by a critical section runs on after deleting itself"
    );
    assert!(
        !progress.after_then.load(Ordering::SeqCst),
        "{case:?}: a deleted task ran on after waiting"
    );
    assert_eq!(
        rounds.load(Ordering::SeqCst),
        5,
        "{case:?}: the healthy peer did not run to completion"
    );
    assert_eq!(
        sim_ffi::freertos::termination_bookkeeping(),
        (0, 0),
        "{case:?}: the idle task did not free every deleted task"
    );
    assert!(
        !sim_ffi::is_critical_locked(),
        "{case:?}: the deleted task's mask outlived it"
    );
}

fn matrix(kind: Kind, then: Then) {
    for hold in [Hold::Critical, Hold::SchedulerLock, Hold::Plain] {
        for bounded in [true, false] {
            run(Case {
                kind,
                hold,
                then,
                bounded,
            });
        }
    }
}

macro_rules! matrix_tests {
    ($($name:ident: $kind:expr, $then:expr;)*) => {
        $(
            #[test]
            fn $name() {
                matrix($kind, $then);
            }
        )*
    };
}

matrix_tests! {
    native_task_faults_after_deleting_itself: Kind::Native, Then::Fault;
    native_task_returns_after_deleting_itself: Kind::Native, Then::Return;
    native_task_yields_and_sleeps_after_deleting_itself: Kind::Native, Then::YieldSleep;
    native_task_uses_up_its_budget_after_deleting_itself: Kind::Native, Then::Budget;
    native_task_unmasks_after_deleting_itself: Kind::Native, Then::Unmask;
    firmware_task_faults_after_deleting_itself: Kind::Firmware, Then::Fault;
    firmware_task_returns_after_deleting_itself: Kind::Firmware, Then::Return;
    firmware_task_yields_and_sleeps_after_deleting_itself: Kind::Firmware, Then::YieldSleep;
    firmware_task_uses_up_its_budget_after_deleting_itself: Kind::Firmware, Then::Budget;
    firmware_task_unmasks_after_deleting_itself: Kind::Firmware, Then::Unmask;
    legacy_task_faults_after_deleting_itself: Kind::Legacy, Then::Fault;
    legacy_task_returns_after_deleting_itself: Kind::Legacy, Then::Return;
    legacy_task_yields_and_sleeps_after_deleting_itself: Kind::Legacy, Then::YieldSleep;
    legacy_task_uses_up_its_budget_after_deleting_itself: Kind::Legacy, Then::Budget;
    legacy_task_unmasks_after_deleting_itself: Kind::Legacy, Then::Unmask;
}

#[cfg(unix)]
matrix_tests! {
    native_task_waits_on_io_after_deleting_itself: Kind::Native, Then::IoWait;
    firmware_task_waits_on_io_after_deleting_itself: Kind::Firmware, Then::IoWait;
    legacy_task_waits_on_io_after_deleting_itself: Kind::Legacy, Then::IoWait;
}
