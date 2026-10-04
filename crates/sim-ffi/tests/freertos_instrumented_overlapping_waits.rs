//! A budget tick deferred during one task's wait setup stays that task's
//! debt, whatever other tasks' waits do meanwhile.
//!
//! Under edge instrumentation (`SIM_INSTRUMENT_EDGES=1`) task A can use up
//! its budget while setting up `sleep_until(10)`; the tick is deferred
//! until the wait ends (see `WaitSetup`).  While A sleeps, other tasks set
//! up their own waits (sleeps, host I/O).  When A wakes at tick 10 it is
//! charged its tick first, so its continuation runs at tick 11.  A deleted
//! while it owes the tick leaves no debt behind.
//!
//! The scenario needs a pad (kernel calls before the wait) at which A's
//! budget runs out inside the wait setup; `tests/golden_trace_test.sh`
//! runs this with `SIM_INSTRUMENT_EDGES=1`.  Without instrumentation no
//! pad defers, and the tests have nothing to check.
#![cfg(unix)]

use std::ffi::{c_char, c_long, c_ulong, c_void};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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
    fn xTaskGetCurrentTaskHandle() -> *mut c_void;
}

unsafe extern "C" fn anchor(_: *mut c_void) {
    loop {
        vTaskDelay(1000);
    }
}

unsafe extern "C" fn reset_budget() {
    sim_ffi::sim_budget_set_limit(1);
    sim_ffi::sim_budget_reset();
}

#[derive(Clone, Copy, Debug)]
enum Other {
    Sleep(u64),
    Io,
}

fn instrumented() -> bool {
    std::env::var_os("SIM_INSTRUMENT_EDGES").is_some_and(|v| v == "1")
}

struct Run {
    sim: Simulator,
    woke: Arc<Mutex<Option<u64>>>,
    handle: Arc<AtomicUsize>,
    _sockets: Vec<UnixStream>,
}

/// Sets up A at `pad` and steps tick 0.  `None` if A's budget did not run
/// out inside its wait setup (no debt).
fn start(pad: u32) -> Option<Run> {
    let mut sim = Simulator::new(SimConfig::default());
    let woke = Arc::new(Mutex::new(None));
    let handle = Arc::new(AtomicUsize::new(0));
    {
        let _active = sim.activate();
        unsafe {
            sim_ffi::sim_budget_set_limit(1_000_000);
            sim_ffi::sim_budget_reset();
            xTaskCreate(
                anchor,
                c"anchor".as_ptr(),
                128,
                std::ptr::null_mut(),
                1,
                std::ptr::null_mut(),
            );
        }
        let (out, handle_out) = (woke.clone(), handle.clone());
        sim_ffi::spawn_rust_task("a", 7, 65536, move |ctx| unsafe {
            handle_out.store(xTaskGetCurrentTaskHandle() as usize, Ordering::SeqCst);
            sim_ffi::sim_budget_set_limit(1_000_000);
            sim_ffi::sim_budget_reset();
            for _ in 0..pad {
                std::hint::black_box(xTaskGetTickCount());
            }
            sim_ffi::sim_budget_set_limit(1);
            sim_ffi::sim_budget_reset();
            ctx.sleep_until(10);
            sim_ffi::sim_budget_set_limit(1_000_000);
            *out.lock().unwrap() = Some(ctx.now());
        });
        sim.set_scheduler_limit(Some(0));
        unsafe { sim_ffi::sim_scheduler_tick() };
        let owes = !sim_ffi::freertos::wait_budget_debts().is_empty();
        if !owes {
            unsafe { sim_ffi::sim_budget_set_limit(1_000_000) };
            return None;
        }
    }
    Some(Run {
        sim,
        woke,
        handle,
        _sockets: Vec::new(),
    })
}

/// The first run (pad) in which A owes a deferred tick.  The edge hook's
/// throttle counts edges process-wide, so the pad that defers depends on
/// what ran before: every scenario searches its own.
fn deferring_run() -> Option<(u32, Run)> {
    (0..4000).find_map(|pad| start(pad).map(|run| (pad, run)))
}

/// A owes its tick, other tasks set up their waits while A sleeps; returns
/// A's wake tick after the World reached tick 10, then tick 11.
fn overlapping(mut run: Run, others: &[Other]) -> (Option<u64>, Option<u64>) {
    let _active = run.sim.activate();
    for (i, &other) in others.iter().enumerate() {
        let priority = 6 - i as u32;
        match other {
            Other::Sleep(until) => {
                sim_ffi::spawn_rust_task("sleeper", priority, 65536, move |ctx| {
                    ctx.sleep_until(until);
                });
            }
            Other::Io => {
                let (reader, writer) = UnixStream::pair().unwrap();
                let fd = reader.as_raw_fd();
                assert_eq!(unsafe { sim_ffi::net_ffi::sim_host_register_fd(fd) }, 0);
                run._sockets.push(reader);
                run._sockets.push(writer);
                sim_ffi::spawn_rust_task("io", priority, 65536, move |_| unsafe {
                    sim_ffi::net_ffi::sim_host_block_on_fd(fd);
                });
            }
        }
    }
    run.sim.set_scheduler_limit(Some(0));
    for _ in 0..3 {
        unsafe { sim_ffi::sim_scheduler_tick() };
    }
    unsafe { sim_ffi::sim_schedule_event(10, Some(reset_budget)) };
    run.sim.set_scheduler_limit(Some(10));
    unsafe { sim_ffi::sim_scheduler_tick() };
    let at_10 = *run.woke.lock().unwrap();
    run.sim.set_scheduler_limit(Some(11));
    unsafe { sim_ffi::sim_scheduler_tick() };
    let at_11 = *run.woke.lock().unwrap();
    for socket in &run._sockets {
        let _ = sim_ffi::net_ffi::sim_host_deregister_fd(socket.as_raw_fd());
    }
    unsafe { sim_ffi::sim_budget_set_limit(1_000_000) };
    (at_10, at_11)
}

#[test]
fn a_deferred_budget_tick_survives_other_tasks_waits() {
    let cases: [&[Other]; 6] = [
        &[],
        &[Other::Sleep(20)],
        &[Other::Io],
        &[Other::Sleep(20), Other::Sleep(30)],
        &[Other::Sleep(20), Other::Io],
        &[Other::Io, Other::Sleep(30)],
    ];
    for others in cases {
        let Some((pad, run)) = deferring_run() else {
            assert!(!instrumented(), "no pad deferred A's budget tick");
            return;
        };
        let (at_10, at_11) = overlapping(run, others);
        assert_eq!(
            at_10, None,
            "pad={pad} others={others:?}: A ran before its deferred tick was charged"
        );
        assert_eq!(at_11, Some(11), "pad={pad} others={others:?}");
    }
}

#[test]
fn a_task_deleted_while_owing_a_deferred_tick_leaves_no_debt() {
    let Some((_, mut run)) = deferring_run() else {
        assert!(!instrumented(), "no pad deferred A's budget tick");
        return;
    };
    let _active = run.sim.activate();
    assert!(!sim_ffi::freertos::wait_budget_debts().is_empty());
    unsafe { vTaskDelete(run.handle.load(Ordering::SeqCst) as *mut c_void) };
    assert!(sim_ffi::freertos::wait_budget_debts().is_empty());
    run.sim.set_scheduler_limit(Some(20));
    unsafe { sim_ffi::sim_scheduler_tick() };
    assert_eq!(*run.woke.lock().unwrap(), None);
    assert!(sim_ffi::freertos::wait_budget_debts().is_empty());
    unsafe { sim_ffi::sim_budget_set_limit(1_000_000) };
}
