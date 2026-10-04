//! Held-off tick interrupts are serviced in one batch, never split by a
//! budget tick.
//!
//! A task keeps interrupts masked until tick 3000, using up its budget all
//! along, then unmasks: the ~3000 held-off ticks are serviced at once, in
//! `xTaskIncrementTick()` calls that, under edge instrumentation
//! (`SIM_INSTRUMENT_EDGES=1`), poll the budget themselves.  A budget tick
//! landing inside that batch used to suspend the task with the kernel's
//! tick count behind the virtual clock, so a callback at 3001 read an old
//! tick and a five-tick timer it started fired at 3002 instead of 3006.
//! The batch now runs without budget preemption, and the tick it used up
//! is charged right after.  `tests/golden_trace_test.sh` runs this with
//! `SIM_INSTRUMENT_EDGES=1`; without instrumentation it checks the plain
//! behaviour.

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;
use std::ffi::{c_char, c_long, c_ulong, c_void};
use std::sync::{Arc, Mutex};
extern "C" {
    fn xTaskCreate(
        entry: unsafe extern "C" fn(*mut c_void),
        name: *const c_char,
        depth: u16,
        arg: *mut c_void,
        priority: c_ulong,
        handle: *mut *mut c_void,
    ) -> c_long;
    fn xTaskGetTickCount() -> u32;
    fn xTimerCreate(
        name: *const c_char,
        period: u32,
        autoreload: c_long,
        id: *mut c_void,
        callback: unsafe extern "C" fn(*mut c_void),
    ) -> *mut c_void;
    fn xTimerGenericCommandFromTask(
        timer: *mut c_void,
        command: c_long,
        value: u32,
        woken: *mut c_long,
        block: u32,
    ) -> c_long;
}
unsafe extern "C" fn anchor(_: *mut c_void) {}
unsafe extern "C" fn timer_fired(_: *mut c_void) {
    sim_ffi::sim_trace_u32(c"review_timer".as_ptr(), xTaskGetTickCount());
}
unsafe extern "C" fn check_clock() {
    let tick = xTaskGetTickCount();
    sim_ffi::sim_trace_u32(c"callback_kernel".as_ptr(), tick);
    let timer = xTimerCreate(c"timer".as_ptr(), 5, 0, std::ptr::null_mut(), timer_fired);
    assert!(!timer.is_null());
    assert_eq!(
        xTimerGenericCommandFromTask(timer, 1, tick, std::ptr::null_mut(), 0),
        1
    );
}
fn unmask_services_ticks_before_preempting(bounded: bool) {
    let mut sim = Simulator::new(SimConfig::default());
    let _active = sim.activate();
    unsafe {
        sim_ffi::sim_budget_set_limit(1_000_000);
        sim_ffi::sim_budget_reset();
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
    let observed = Arc::new(Mutex::new(None));
    let out = observed.clone();
    sim_ffi::spawn_rust_task("high", 7, 65536, move |ctx| unsafe {
        ctx.sleep_until(5);
        sim_ffi::sim_budget_set_limit(1_000_000);
        *out.lock().unwrap() = Some((ctx.now(), xTaskGetTickCount()));
    });
    sim_ffi::spawn_rust_task("low", 6, 65536, move |ctx| unsafe {
        sim_ffi::freertos::sim_disable_interrupts();
        sim_ffi::sim_budget_set_limit(1);
        sim_ffi::sim_budget_reset();
        while ctx.now() < 3000 {
            sim_ffi::sim_budget_poll(std::ptr::null(), 0);
        }
        sim_ffi::freertos::sim_enable_interrupts();
        sim_ffi::sim_budget_set_limit(1_000_000);
    });
    unsafe {
        sim_ffi::sim_schedule_event(3001, Some(check_clock));
    }
    if !bounded {
        sim.set_scheduler_limit(None);
        for _ in 0..10000 {
            if unsafe { sim_ffi::sim_scheduler_tick() } == 0 {
                break;
            }
        }
    } else {
        sim.set_scheduler_limit(Some(4000));
        unsafe {
            sim_ffi::sim_scheduler_tick();
        }
    }
    unsafe {
        sim_ffi::sim_budget_set_limit(1_000_000);
    }
    let clocks: Vec<_> = sim
        .sim_global
        .borrow()
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match e {
            sim_core::trace::TraceEvent::UserU32 {
                at,
                label: "callback_kernel",
                value,
            } => Some((*at, *value)),
            _ => None,
        })
        .collect();
    let timers: Vec<_> = sim
        .sim_global
        .borrow()
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match e {
            sim_core::trace::TraceEvent::UserU32 {
                at,
                label: "review_timer",
                value,
            } => Some((*at, *value)),
            _ => None,
        })
        .collect();
    eprintln!("callback clocks {clocks:?}, timer fired {timers:?}");
    assert_eq!(timers, vec![(3006, 3006)]);
    let result = observed.lock().unwrap().unwrap();
    eprintln!("unmask observed {result:?}");
    assert_eq!(
        result.0, result.1 as u64,
        "high task ran before held ticks finished servicing"
    );
}

#[test]
fn held_ticks_are_serviced_in_one_batch_bounded() {
    unmask_services_ticks_before_preempting(true);
}

#[test]
fn held_ticks_are_serviced_in_one_batch_unbounded() {
    unmask_services_ticks_before_preempting(false);
}
