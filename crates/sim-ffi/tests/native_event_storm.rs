//! A native-only Simulator and peripheral callbacks: virtual time never
//! runs backward, a finite burst within the storm limit runs at its tick,
//! and a burst past it stops the machine as an interrupt storm instead of
//! hanging or deferring work.

use sim_core::{SimConfig, TraceEvent};
use sim_ffi::simulator::Simulator;
use std::sync::Mutex;

static CALLBACK_TICKS: Mutex<Vec<u64>> = Mutex::new(Vec::new());
static SLEEPER_TICKS: Mutex<Vec<u64>> = Mutex::new(Vec::new());
/// Both tests use the statics above.
static FIXTURE: Mutex<()> = Mutex::new(());

unsafe extern "C" fn record() {
    CALLBACK_TICKS
        .lock()
        .unwrap()
        .push(sim_ffi::sim_now_ticks());
}

/// `callbacks` callbacks at tick 1 and a task sleeping until 10, then 20,
/// stepped until done.  Returns (callback ticks, sleeper ticks, events).
fn run(callbacks: usize) -> (Vec<u64>, Vec<u64>, Vec<TraceEvent>, bool) {
    CALLBACK_TICKS.lock().unwrap().clear();
    SLEEPER_TICKS.lock().unwrap().clear();
    let mut sim = Simulator::new(SimConfig::default());
    let g = sim.sim_global.clone();
    let _a = sim.activate();
    for _ in 0..callbacks {
        unsafe { sim_ffi::sim_schedule_event(1, Some(record)) };
    }
    sim_ffi::spawn_rust_task("sleeper", 1, 4096, |ctx| {
        ctx.sleep_until(10);
        SLEEPER_TICKS.lock().unwrap().push(ctx.now());
        ctx.sleep_until(20);
        SLEEPER_TICKS.lock().unwrap().push(ctx.now());
    });
    let mut last = 0;
    let mut done = false;
    for _ in 0..1_000 {
        let more = unsafe { sim_ffi::sim_scheduler_tick() } != 0;
        let now = g.borrow().scheduler_sim_time;
        assert!(now >= last, "time ran backward: {last} -> {now}");
        last = now;
        if !more {
            done = true;
            break;
        }
    }
    let halted = sim_ffi::freertos::halted();
    assert!(done, "the scheduler never completed");
    let events = g.borrow().trace.as_ref().unwrap().events.clone();
    (
        CALLBACK_TICKS.lock().unwrap().clone(),
        SLEEPER_TICKS.lock().unwrap().clone(),
        events,
        halted,
    )
}

#[test]
fn finite_callback_burst_runs_at_its_tick() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    let (ticks, sleeper, _, halted) = run(1000);
    assert_eq!(ticks, vec![1; 1000]);
    assert_eq!(sleeper, vec![10, 20]);
    assert!(!halted);
}

#[test]
fn callback_burst_past_the_storm_limit_stops_the_machine() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    let (ticks, sleeper, events, halted) = run(3000);
    // The limit (1024) is exceeded at tick 1: no callback runs later, and
    // none at an earlier tick than its deadline.
    assert!(ticks.iter().all(|&t| t == 1));
    assert!(ticks.len() <= 1025, "{} callbacks ran", ticks.len());
    assert!(sleeper.is_empty(), "the stopped machine kept running");
    assert!(halted);
    let storms: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 {
                at,
                label: "irq_storm",
                ..
            } => Some(*at),
            _ => None,
        })
        .collect();
    assert_eq!(storms, vec![1]);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, TraceEvent::Fatal { .. }))
            .count(),
        1
    );
}
