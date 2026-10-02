//! A native-only Simulator under a peripheral-callback storm: callbacks
//! the per-tick bound leaves queued run at the next tick, before any later
//! task deadline, and virtual time never runs backward.

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;
use std::sync::Mutex;

static CALLBACK_TICKS: Mutex<Vec<u64>> = Mutex::new(Vec::new());
static SLEEPER_TICKS: Mutex<Vec<u64>> = Mutex::new(Vec::new());

unsafe extern "C" fn record() {
    CALLBACK_TICKS
        .lock()
        .unwrap()
        .push(sim_ffi::sim_now_ticks());
}

#[test]
fn deferred_callbacks_run_next_tick_and_time_is_monotonic() {
    let mut sim = Simulator::new(SimConfig::default());
    let g = sim.sim_global.clone();
    let _a = sim.activate();
    for _ in 0..3000 {
        unsafe { sim_ffi::sim_schedule_event(1, Some(record)) };
    }
    sim_ffi::spawn_rust_task("sleeper", 1, 4096, |ctx| {
        ctx.sleep_until(10);
        SLEEPER_TICKS.lock().unwrap().push(ctx.now());
        ctx.sleep_until(20);
        SLEEPER_TICKS.lock().unwrap().push(ctx.now());
    });
    let mut last = 0;
    for _ in 0..1_000 {
        let more = unsafe { sim_ffi::sim_scheduler_tick() } != 0;
        let now = g.borrow().scheduler_sim_time;
        assert!(now >= last, "time ran backward: {last} -> {now}");
        last = now;
        if !more {
            break;
        }
    }
    let ticks = CALLBACK_TICKS.lock().unwrap().clone();
    assert_eq!(ticks.len(), 3000);
    assert!(
        ticks.windows(2).all(|w| w[0] <= w[1]),
        "callbacks ran out of order"
    );
    // 1024 per stalled tick, starting at their deadline.
    let per_tick = |t| ticks.iter().filter(|&&x| x == t).count();
    assert_eq!((per_tick(1), per_tick(2), per_tick(3)), (1024, 1024, 952));
    assert_eq!(*SLEEPER_TICKS.lock().unwrap(), vec![10, 20]);
}
