//! After a fatal interrupt storm, the machine is stopped for good on every
//! backend and for every storm source.
//!
//! Backends: native (the engine's own scheduler), FreeRTOS stepped
//! standalone (unbounded) and bounded (World-style, with a scheduler
//! limit), the Zephyr scheduler loop (`sim_zephyr_start_scheduler`) and
//! step (`sim_zephyr_scheduler_tick`), and a World running a native or a
//! FreeRTOS machine next to a healthy neighbour.  Sources: an IRQ raised by
//! a task (the storm is delivered on the task's fiber), an IRQ taken in
//! scheduler context whose ISR keeps re-raising it, a peripheral callback
//! rescheduling itself for now, and a timer ISR re-arming its timer with
//! zero delay.
//!
//! For each case: (a) no guest code (task, ISR, callback) runs once the
//! machine has stopped, (b) the machine asks for no further wakes and later
//! steps do nothing, (c) in a World the other machine keeps running, and
//! (d) the scheduler call returns (each case runs on its own thread, with a
//! deadline).
//!
//! A timer re-armed with zero delay is a storm only where the timer expiry
//! is a scheduling deadline (FreeRTOS).  The native and Zephyr schedulers
//! do not wait for timers: each ISR runs at one scheduling step, time moves
//! with the tasks, and no tick is ever stuck, so they have no timer case.

use std::cell::Cell;
use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use sim_core::{SimConfig, Tick, TraceEvent};
use sim_ffi::device_ffi::{sim_irq_raise, sim_irq_set_handler, sim_timer_arm};
use sim_ffi::simulator::Simulator;
use sim_world::firmware::Firmware;
use sim_world::machine::Machine;
use sim_world::world::World;

extern "C" {
    fn costar_test_spawn_task(name: *const c_char, body: extern "C" fn(), priority: u32);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// A task raises the IRQ; its ISR re-raises it.
    TaskIrq,
    /// The IRQ arrives in scheduler context, while every task waits; its
    /// ISR re-raises it.
    IsrIrq,
    /// Like `IsrIrq`, but the IRQ is already due when the first step
    /// starts (host input between steps), with a task ready to run.
    EntryIrq,
    /// A callback reschedules itself for the current tick.
    Callback,
    /// A timer ISR re-arms its timer with zero delay.
    Timer,
}

const ALL_SOURCES: [Source; 5] = [
    Source::TaskIrq,
    Source::IsrIrq,
    Source::EntryIrq,
    Source::Callback,
    Source::Timer,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Native,
    FreeRtosStandalone,
    FreeRtosBounded,
    ZephyrLoop,
    ZephyrTick,
}

impl Backend {
    fn freertos(self) -> bool {
        matches!(self, Backend::FreeRtosStandalone | Backend::FreeRtosBounded)
    }

    fn zephyr(self) -> bool {
        matches!(self, Backend::ZephyrLoop | Backend::ZephyrTick)
    }

    /// The sources that are interrupt storms on this backend.
    fn sources(self) -> Vec<Source> {
        ALL_SOURCES
            .into_iter()
            .filter(|&s| s != Source::Timer || self.freertos())
            .collect()
    }
}

impl Source {
    /// The tick at which the storm starts.
    fn storm_tick(self) -> Tick {
        if self == Source::EntryIrq {
            0
        } else {
            STORM_AT
        }
    }
}

const STORM_IRQ: u32 = 9;
const TIMER_IRQ: u32 = 6;
/// The tick at which every storm but `EntryIrq` starts.
const STORM_AT: Tick = 2;
/// Storm limit of the storming machine (the World test uses the default).
const STORM_LIMIT: u32 = 16;

thread_local! {
    static SOURCE: Cell<Source> = const { Cell::new(Source::TaskIrq) };
    /// Units of guest code (task steps, ISRs, callbacks) the storming
    /// machine ran.
    static RAN: Cell<u32> = const { Cell::new(0) };
    /// Units of guest code it ran while stopped.
    static RAN_AFTER_HALT: Cell<u32> = const { Cell::new(0) };
    /// Steps of the healthy neighbour's task.
    static NEIGHBOUR: Cell<u32> = const { Cell::new(0) };
}

/// One unit of the storming machine's guest code.
fn guest() {
    RAN.with(|r| r.set(r.get() + 1));
    if sim_ffi::freertos::halted() {
        RAN_AFTER_HALT.with(|r| r.set(r.get() + 1));
    }
}

fn delay(ticks: Tick) {
    unsafe { sim_ffi::sim_task_delay_until(sim_ffi::sim_now_ticks() + ticks) };
}

/// The task that starts a `TaskIrq` storm at [`STORM_AT`]; otherwise a
/// periodic task like the bystander.
extern "C" fn stormer() {
    guest();
    unsafe { sim_ffi::sim_task_delay_until(STORM_AT) };
    guest();
    if SOURCE.with(Cell::get) == Source::TaskIrq {
        unsafe { sim_irq_raise(STORM_IRQ) };
        // The storm was delivered on this task's fiber: the machine
        // stopped inside `sim_irq_raise()`, so this must never run.
        guest();
    }
    loop {
        delay(1);
        guest();
    }
}

/// A lower-priority task with work every tick, ready at once: it must stop
/// with the machine (and keeps the native scheduler's sleep deadlines
/// alive).
extern "C" fn bystander() {
    loop {
        guest();
        delay(1);
    }
}

unsafe extern "C" fn retrigger_isr() {
    guest();
    sim_irq_raise(STORM_IRQ);
}

unsafe extern "C" fn rearm_isr() {
    guest();
    sim_timer_arm(0, 0);
}

unsafe extern "C" fn reschedule_now() {
    guest();
    sim_ffi::sim_schedule_event(sim_ffi::sim_now_ticks(), Some(reschedule_now));
}

unsafe extern "C" fn zephyr_stormer(_: *mut c_void, _: *mut c_void, _: *mut c_void) {
    stormer();
}

unsafe extern "C" fn zephyr_bystander(_: *mut c_void, _: *mut c_void, _: *mut c_void) {
    bystander();
}

/// Create the storming machine's tasks, ISRs and storm source, with the
/// machine active.
fn setup(source: Source, backend: Backend) {
    SOURCE.with(|s| s.set(source));
    RAN.with(|r| r.set(0));
    RAN_AFTER_HALT.with(|r| r.set(0));
    unsafe {
        sim_irq_set_handler(STORM_IRQ, Some(retrigger_isr));
        sim_irq_set_handler(TIMER_IRQ, Some(rearm_isr));
    }
    match source {
        Source::TaskIrq => {}
        Source::IsrIrq | Source::EntryIrq => {
            sim_devices::irq::with_irq_mut(|c| c.raise_at(STORM_IRQ, source.storm_tick()));
        }
        Source::Callback => unsafe {
            sim_ffi::sim_schedule_event(STORM_AT, Some(reschedule_now));
        },
        Source::Timer => {
            sim_devices::timer_insert(sim_devices::VirtualTimer::new_oneshot(0, TIMER_IRQ));
            unsafe { sim_timer_arm(0, STORM_AT) };
        }
    }
    if backend.freertos() {
        unsafe {
            costar_test_spawn_task(c"stormer".as_ptr(), stormer, 2);
            costar_test_spawn_task(c"bystander".as_ptr(), bystander, 1);
        }
    } else if backend.zephyr() {
        for (name, entry, priority) in [
            (
                c"stormer",
                zephyr_stormer as unsafe extern "C" fn(_, _, _),
                2,
            ),
            (c"bystander", zephyr_bystander, 1),
        ] {
            unsafe {
                sim_ffi::zephyr_ffi::sim_zephyr_register_thread(
                    name.as_ptr(),
                    Some(entry),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    4096,
                    priority,
                );
            }
        }
    } else {
        sim_ffi::spawn_rust_task("stormer", 2, 4096, |_| stormer());
        sim_ffi::spawn_rust_task("bystander", 1, 4096, |_| bystander());
    }
}

/// Run `f` on a thread of its own (fresh thread-local scheduler state) and
/// fail if it does not return in time: a hang.
fn on_own_thread<T: Send + 'static>(case: String, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name(case.clone())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .unwrap();
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(out) => {
            handle.join().unwrap();
            out
        }
        // The case panicked: report its assertion.
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            std::panic::resume_unwind(handle.join().unwrap_err())
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("{case}: hung"),
    }
}

fn storm_ticks(events: &[TraceEvent]) -> Vec<Tick> {
    events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 {
                at,
                label: "irq_storm",
                ..
            } => Some(*at),
            _ => None,
        })
        .collect()
}

fn fatals(events: &[TraceEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, TraceEvent::Fatal { .. }))
        .count()
}

/// Upper bound on scheduler steps before a case must have stopped.
const MAX_STEPS: u64 = 100_000;

/// One standalone case: run until the machine stops, then check it stays
/// stopped.
fn standalone_case(backend: Backend, source: Source) {
    let case = format!("{backend:?}/{source:?}");
    on_own_thread(case.clone(), move || {
        let mut sim = Simulator::new(SimConfig::default());
        sim.enable_owned_devices();
        sim.set_storm_limit(STORM_LIMIT);
        let g = sim.sim_global.clone();
        let _active = sim.activate();
        setup(source, backend);
        let bounded = backend == Backend::FreeRtosBounded;
        let tick = || unsafe {
            if backend == Backend::ZephyrTick {
                sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick()
            } else {
                sim_ffi::sim_scheduler_tick()
            }
        };

        // (d) The scheduler returns, and reports completion.
        if backend == Backend::ZephyrLoop {
            unsafe { sim_ffi::zephyr_ffi::sim_zephyr_start_scheduler() };
        } else {
            let mut done = false;
            for step in 0..MAX_STEPS {
                if bounded {
                    sim.set_scheduler_limit(Some(step));
                }
                if tick() == 0 && (!bounded || sim_ffi::freertos::halted()) {
                    done = true;
                    break;
                }
            }
            assert!(done, "{case}: the scheduler never reported completion");
        }
        assert!(sim_ffi::freertos::halted(), "{case}: no storm stopped it");
        assert!(RAN.with(Cell::get) > 0, "{case}: the storm never started");

        // (b) Stopped for good: later steps run nothing, move no time and
        // report completion; nothing asks for a wake.
        let time = g.borrow().scheduler_sim_time;
        let ran = RAN.with(Cell::get);
        for step in 0..100 {
            if bounded {
                sim.set_scheduler_limit(Some(1_000 + step));
            }
            if backend == Backend::ZephyrLoop {
                unsafe { sim_ffi::zephyr_ffi::sim_zephyr_start_scheduler() };
            } else {
                assert_eq!(tick(), 0, "{case}: a later step reported more work");
            }
        }
        assert_eq!(g.borrow().scheduler_sim_time, time, "{case}: time moved");
        assert_eq!(RAN.with(Cell::get), ran, "{case}: guest code ran later");
        assert!(sim.halted(), "{case}");
        if backend.freertos() {
            assert_eq!(sim.freertos_pending_work_tick(), None, "{case}: wake");
        }

        // (a) No guest code ran once the machine had stopped.
        assert_eq!(
            RAN_AFTER_HALT.with(Cell::get),
            0,
            "{case}: guest code ran after the storm stopped the machine"
        );

        // One storm report and one fault, at the storm's tick.
        sim_ffi::flush_trace();
        let events = g.borrow().trace.as_ref().unwrap().events.clone();
        assert_eq!(storm_ticks(&events), vec![source.storm_tick()], "{case}");
        assert_eq!(fatals(&events), 1, "{case}");
    });
}

/// Run `case` for every storm source of `backend`; every case runs, and
/// the failing ones are listed (their assertions are printed above).
fn each_source(backend: Backend, case: impl Fn(Backend, Source)) {
    let failed: Vec<Source> = backend
        .sources()
        .into_iter()
        .filter(|&source| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| case(backend, source)))
                .is_err()
        })
        .collect();
    assert!(failed.is_empty(), "{backend:?}: failed for {failed:?}");
}

fn standalone_matrix(backend: Backend) {
    each_source(backend, standalone_case);
}

#[test]
fn native_scheduler_stays_stopped_after_a_storm() {
    standalone_matrix(Backend::Native);
}

#[test]
fn standalone_freertos_stays_stopped_after_a_storm() {
    standalone_matrix(Backend::FreeRtosStandalone);
}

#[test]
fn bounded_freertos_stays_stopped_after_a_storm() {
    standalone_matrix(Backend::FreeRtosBounded);
}

#[test]
fn zephyr_scheduler_loop_stays_stopped_after_a_storm() {
    standalone_matrix(Backend::ZephyrLoop);
}

#[test]
fn zephyr_scheduler_step_stays_stopped_after_a_storm() {
    standalone_matrix(Backend::ZephyrTick);
}

// ── World ──────────────────────────────────────────────────────────────

/// The storming machine's firmware: boots the case and runs the scheduler
/// on every step.  Counts its steps.
struct StormFirmware {
    backend: Backend,
    source: Source,
    steps: Arc<AtomicU32>,
}

impl Firmware for StormFirmware {
    fn init(&mut self, machine: &mut Machine) {
        let _active = machine.activate();
        setup(self.source, self.backend);
    }

    fn step(&mut self, _now: Tick, machine: &mut Machine) {
        self.steps.fetch_add(1, Ordering::SeqCst);
        let _active = machine.activate();
        unsafe { sim_ffi::sim_scheduler_tick() };
        sim_ffi::flush_trace();
    }
}

/// The healthy neighbour: a native task with work every tick.
struct NeighbourFirmware;

impl Firmware for NeighbourFirmware {
    fn init(&mut self, machine: &mut Machine) {
        let _active = machine.activate();
        sim_ffi::spawn_rust_task("neighbour", 1, 4096, |ctx| loop {
            ctx.sleep_for(1);
            NEIGHBOUR.with(|n| n.set(n.get() + 1));
        });
    }

    fn step(&mut self, _now: Tick, machine: &mut Machine) {
        let _active = machine.activate();
        unsafe { sim_ffi::sim_scheduler_tick() };
        sim_ffi::flush_trace();
    }
}

/// World time run by each World case, in µs (20 firmware ticks).
const WORLD_US: Tick = 20_000;

fn world_case(backend: Backend, source: Source) {
    let case = format!("World/{backend:?}/{source:?}");
    on_own_thread(case.clone(), move || {
        NEIGHBOUR.with(|n| n.set(0));
        let steps = Arc::new(AtomicU32::new(0));
        let mut world = World::new();
        world.enable_owned_device_banks();
        for (id, name) in [(1, "storm"), (2, "neighbour")] {
            let mut machine = Machine::with_defaults(id, name);
            machine.schedule_at(0, 0, "boot", Box::new(|_| {}));
            world.add_machine(machine);
        }
        world
            .machine_mut(1)
            .unwrap()
            .load_firmware(Box::new(StormFirmware {
                backend,
                source,
                steps: steps.clone(),
            }));
        world
            .machine_mut(2)
            .unwrap()
            .load_firmware(Box::new(NeighbourFirmware));
        // (d) The World run returns.
        world.run_until(WORLD_US).unwrap();

        let lines = world.drain_all_traces();
        let storm = lines
            .iter()
            .filter(|l| l.starts_with("[machine.1]") && l.contains("\"irq_storm\""))
            .count();
        let fatal = lines
            .iter()
            .filter(|l| l.starts_with("[machine.1]") && l.contains("FATAL"))
            .count();
        assert_eq!((storm, fatal), (1, 1), "{case}: one storm, one fault");
        // (a) No guest code ran on the stopped machine, though the World
        // stepped it again for its neighbour.
        assert!(RAN.with(Cell::get) > 0, "{case}: the storm never started");
        assert_eq!(
            RAN_AFTER_HALT.with(Cell::get),
            0,
            "{case}: guest code ran after the storm stopped the machine"
        );
        // (b) No busy wake: the stopped machine is only stepped along with
        // its neighbour's World steps (a few per tick), never every µs, and
        // it asks for no further wake.
        let steps = steps.load(Ordering::SeqCst);
        assert!(
            steps <= 10 * (WORLD_US / 1_000) as u32,
            "{case}: the stopped machine was stepped {steps} times in {WORLD_US} us"
        );
        assert_eq!(
            world.machine(1).unwrap().next_event_time(),
            None,
            "{case}: the stopped machine still asks for a wake"
        );
        // (c) The neighbour kept running to the end.
        let neighbour = NEIGHBOUR.with(Cell::get);
        assert!(
            neighbour >= (WORLD_US / 1_000 - 2) as u32,
            "{case}: the neighbour ran only {neighbour} times"
        );
    });
}

#[test]
fn world_native_machine_stays_stopped_after_a_storm_and_the_world_runs_on() {
    each_source(Backend::Native, world_case);
}

#[test]
fn world_freertos_machine_stays_stopped_after_a_storm_and_the_world_runs_on() {
    each_source(Backend::FreeRtosBounded, world_case);
}
