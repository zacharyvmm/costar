//! The interrupt-mask semantics of a FreeRTOS machine (docs/scheduling.md,
//! "Interrupt masking"), checked as one property over every masked work
//! source, stepped bounded (World-style) and unbounded:
//!
//! - kept while masked: virtual time, the CPU budget of the running task
//!   (a busy masked task still uses up time), peripheral callbacks at their
//!   deadlines;
//! - deferred until the unmask: kernel tick servicing (a delayed task does
//!   not wake), task switches, and (with #14) IRQ delivery;
//! - no wake-up is reported for deferred work.

use std::cell::RefCell;
use std::ffi::{c_char, c_long, c_ulong, c_void};

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
    fn vTaskDelay(ticks: u32);
    fn vTaskDelete(task: *mut c_void);
    fn xTaskGetTickCount() -> u32;
    fn costar_test_abi_delay_boot();
}

thread_local! {
    /// `(label, firmware time, kernel tick count, masked)` records.
    static LOG: RefCell<Vec<(&'static str, u64, u32, bool)>> = const { RefCell::new(Vec::new()) };
}

fn log(label: &'static str) {
    let kernel = unsafe { xTaskGetTickCount() };
    LOG.with(|l| {
        l.borrow_mut().push((
            label,
            unsafe { sim_ffi::sim_now_ticks() },
            kernel,
            sim_ffi::is_critical_locked(),
        ))
    });
}

fn take_log() -> Vec<(&'static str, u64, u32, bool)> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

unsafe extern "C" fn sleeper(_: *mut c_void) {
    vTaskDelay(5);
    log("sleeper_woke");
    vTaskDelete(std::ptr::null_mut());
}

/// A task that masks interrupts and busy-waits for tick 3.
unsafe extern "C" fn busy(_: *mut c_void) {
    sim_ffi::sim_budget_set_limit(2);
    sim_ffi::sim_budget_reset();
    sim_ffi::sim_enter_critical();
    while sim_ffi::sim_now_ticks() < 3 {
        sim_ffi::sim_budget_poll(std::ptr::null(), 1);
    }
    log("busy_done");
    sim_ffi::sim_exit_critical();
    log("busy_unmasked");
    vTaskDelete(std::ptr::null_mut());
}

unsafe extern "C" fn callback() {
    log("callback");
}

fn create(entry: unsafe extern "C" fn(*mut c_void), name: &std::ffi::CStr, priority: c_ulong) {
    let created = unsafe {
        xTaskCreate(
            entry,
            name.as_ptr(),
            256,
            std::ptr::null_mut(),
            priority,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(created, 1);
}

/// One scheduler call per tick from `from` to `until`, bounded at that
/// tick (World-style) or not.  Returns how many calls reported more work.
fn step(sim: &Simulator, bounded: bool, from: u64, until: u64) -> u32 {
    let mut more = 0;
    for t in from..=until {
        sim.set_scheduler_limit(bounded.then_some(t));
        more += unsafe { sim_ffi::sim_scheduler_tick() };
    }
    more
}

/// A busy task that masks interrupts still uses up CPU time: it reaches
/// tick 3 (the kernel has counted none of it), and its unmask services the
/// held-off ticks.  The higher-priority sleeper due at 5 waits.
#[test]
fn a_masked_busy_task_keeps_time_moving() {
    for bounded in [true, false] {
        take_log();
        let mut sim = Simulator::new(SimConfig::default());
        let _a = sim.activate();
        create(busy, c"busy", 1);
        step(&sim, bounded, 0, 10);
        let log = take_log();
        assert_eq!(
            log,
            vec![("busy_done", 3, 0, true), ("busy_unmasked", 3, 3, false)],
            "bounded={bounded}"
        );
    }
}

/// An idle machine that host code masks: time moves, callbacks run at
/// their deadlines (masked), the kernel tick and the sleeper wait, and no
/// wake-up is reported for them; at the unmask the held-off ticks are
/// serviced and the sleeper runs then.
#[test]
fn a_masked_idle_machine_runs_callbacks_and_defers_ticks() {
    for bounded in [true, false] {
        let case = format!("bounded={bounded}");
        take_log();
        let mut sim = Simulator::new(SimConfig::default());
        let _a = sim.activate();
        create(sleeper, c"sleeper", 2);
        // The sleeper blocks until tick 5.
        sim.set_scheduler_limit(Some(0));
        unsafe { sim_ffi::sim_scheduler_tick() };
        unsafe { sim_ffi::sim_schedule_event(2, Some(callback)) };
        sim_ffi::freertos::sim_disable_interrupts();
        step(&sim, bounded, 1, 1);
        if bounded {
            // Only the callback can do anything before the unmask.
            assert_eq!(sim.freertos_next_wake(), Some(2), "{case}");
        }
        step(&sim, bounded, 2, 8);
        let now = sim.scheduler_sim_time();
        // The callback ran on time, masked; nothing else ran.
        assert_eq!(take_log(), vec![("callback", 2, 0, true)], "{case}");
        if bounded {
            // Firmware time kept up with the World, and the step reports
            // no wake-up for the deferred kernel deadline.
            assert_eq!(now, 8, "{case}");
            assert_eq!(sim.freertos_next_wake(), None, "{case}");
        } else {
            // Unbounded, time moved only for the callback.
            assert_eq!(now, 2, "{case}");
        }
        // At the unmask the ticks are serviced and the sleeper runs.
        sim_ffi::freertos::sim_enable_interrupts();
        step(&sim, bounded, 8, 9);
        let woke = take_log();
        assert_eq!(woke.len(), 1, "{case}: {woke:?}");
        let (label, at, _, masked) = woke[0];
        assert_eq!((label, masked), ("sleeper_woke", false), "{case}");
        if bounded {
            // Exactly at the unmask: the held-off ticks are serviced at
            // the next step's entry, at tick 8.
            assert_eq!(at, 8, "{case}");
        } else {
            assert_eq!(at, 5, "{case}");
        }
    }
}

/// The reviewer's probe: a native Rust task FreeRTOS schedules, masked,
/// with budget 2, polls six times: three ticks of CPU time.
#[test]
fn a_masked_native_task_on_freertos_is_charged_cpu_time() {
    let mut sim = Simulator::new(SimConfig::default());
    let _a = sim.activate();
    unsafe { costar_test_abi_delay_boot() };
    let measured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let result = measured.clone();
    sim_ffi::spawn_rust_task("masked_busy", 7, 65536, move |ctx| unsafe {
        sim_ffi::sim_budget_set_limit(2);
        sim_ffi::sim_budget_reset();
        sim_ffi::sim_enter_critical();
        for _ in 0..6 {
            sim_ffi::sim_budget_poll(std::ptr::null(), 1);
        }
        *result.lock().unwrap() = Some(ctx.now());
        sim_ffi::sim_exit_critical();
    });
    for _ in 0..30 {
        unsafe { sim_ffi::sim_scheduler_tick() };
    }
    assert_eq!(*measured.lock().unwrap(), Some(3));
}

unsafe extern "C" fn probe_peripheral() {
    log("probe_peripheral");
    sim_ffi::device_ffi::sim_irq_raise(77);
}

/// The reviewer's probe: a callback due at tick 2 runs at tick 2 although
/// interrupts are masked, not at the unmask.
#[test]
fn a_masked_idle_step_dispatches_a_callback_on_time() {
    take_log();
    let mut sim = Simulator::new(SimConfig::default());
    sim.enable_owned_devices();
    let _a = sim.activate();
    unsafe { costar_test_abi_delay_boot() };
    sim.set_scheduler_limit(Some(0));
    unsafe { sim_ffi::sim_scheduler_tick() };
    unsafe { sim_ffi::sim_schedule_event(2, Some(probe_peripheral)) };
    sim_ffi::freertos::sim_disable_interrupts();
    sim.set_scheduler_limit(Some(3));
    unsafe { sim_ffi::sim_scheduler_tick() };
    sim_ffi::freertos::sim_enable_interrupts();
    unsafe { sim_ffi::sim_scheduler_tick() };
    let fired: Vec<_> = take_log()
        .into_iter()
        .filter(|r| r.0 == "probe_peripheral")
        .map(|(_, at, _, masked)| (at, masked))
        .collect();
    assert_eq!(fired, vec![(2, true)]);
}

extern "C" {
    fn xTimerCreate(
        name: *const c_char,
        period: u32,
        reload: c_long,
        id: *mut c_void,
        cb: unsafe extern "C" fn(*mut c_void),
    ) -> *mut c_void;
    fn xTimerGenericCommandFromTask(
        timer: *mut c_void,
        command: c_long,
        value: u32,
        woken: *mut c_long,
        wait: u32,
    ) -> c_long;
}

thread_local! {
    static TIMER: std::cell::Cell<*mut c_void> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}

unsafe extern "C" fn timer_fired(_: *mut c_void) {
    log("timer");
}

/// Unmasks, then starts a 5-tick software timer.
unsafe extern "C" fn unmask_and_start_timer() {
    sim_ffi::freertos::sim_enable_interrupts();
    log("unmask_callback");
    // xTimerStart(): tmrCOMMAND_START = 1.
    let started = TIMER.with(|t| {
        xTimerGenericCommandFromTask(t.get(), 1, xTaskGetTickCount(), std::ptr::null_mut(), 0)
    });
    assert_eq!(started, 1);
}

/// The reviewer's probe: interrupts masked from tick 0, a callback at tick
/// 10 unmasks and starts a 5-tick timer.  The unmask services the held-off
/// ticks at once, so the callback reads kernel tick 10 and the timer fires
/// at 15, as without the mask.
#[test]
fn an_unmask_in_a_callback_services_the_held_off_ticks_at_once() {
    for (masked, bounded) in [(true, true), (true, false), (false, true)] {
        let case = format!("masked={masked} bounded={bounded}");
        take_log();
        let mut sim = Simulator::new(SimConfig::default());
        let _a = sim.activate();
        unsafe { costar_test_abi_delay_boot() };
        sim.set_scheduler_limit(Some(0));
        unsafe { sim_ffi::sim_scheduler_tick() };
        TIMER.with(|t| {
            t.set(unsafe {
                xTimerCreate(c"timer".as_ptr(), 5, 0, std::ptr::null_mut(), timer_fired)
            })
        });
        if masked {
            sim_ffi::freertos::sim_disable_interrupts();
        }
        unsafe { sim_ffi::sim_schedule_event(10, Some(unmask_and_start_timer)) };
        sim.set_scheduler_limit(bounded.then_some(20));
        for _ in 0..50 {
            if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
                break;
            }
        }
        let log: Vec<_> = take_log()
            .into_iter()
            .map(|(label, at, kernel, _)| (label, at, kernel))
            .collect();
        assert_eq!(
            log,
            vec![("unmask_callback", 10, 10), ("timer", 15, 15)],
            "{case}"
        );
    }
}

unsafe extern "C" fn due_now() {
    log("due_now");
}

/// The reviewer's probe: a busy task used up its budget at bounded tick 0
/// (a tick is owed); a callback scheduled for tick 0 and a step with limit
/// 0 run the callback at tick 0 without resuming the task — masked or not.
#[test]
fn a_callback_due_while_a_tick_is_owed_runs_on_time() {
    for masked in [false, true] {
        take_log();
        let mut sim = Simulator::new(SimConfig::default());
        let _a = sim.activate();
        unsafe { costar_test_abi_delay_boot() };
        sim.set_scheduler_limit(Some(0));
        unsafe { sim_ffi::sim_scheduler_tick() };
        sim_ffi::spawn_rust_task("busy", 7, 65536, move |ctx| unsafe {
            if masked {
                sim_ffi::sim_enter_critical();
            }
            sim_ffi::sim_budget_set_limit(1);
            while ctx.now() < 3 {
                sim_ffi::sim_budget_poll(std::ptr::null(), 0);
            }
            log("busy_done");
            if masked {
                sim_ffi::sim_exit_critical();
            }
        });
        // The busy task uses up its budget at tick 0: a tick is owed.
        unsafe { sim_ffi::sim_scheduler_tick() };
        assert_eq!(sim.freertos_next_wake(), Some(1), "masked={masked}");
        unsafe { sim_ffi::sim_schedule_event(0, Some(due_now)) };
        unsafe { sim_ffi::sim_scheduler_tick() };
        let log: Vec<_> = take_log()
            .into_iter()
            .map(|(label, at, kernel, _)| (label, at, kernel))
            .collect();
        assert_eq!(log, vec![("due_now", 0, 0)], "masked={masked}");
        // The task did not run: the tick is still owed.
        assert_eq!(sim.freertos_next_wake(), Some(1), "masked={masked}");
    }
}
