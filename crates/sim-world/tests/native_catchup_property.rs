//! Native (and Zephyr-step) firmware under a World handles every deadline
//! exactly once, at its own tick, in deadline order — IRQ arrivals,
//! peripheral callbacks and sleepers' wake-ups — whether a task keeps the
//! machine busy (yielding forever) or the machine is idle.
//!
//! Work is staged on a grid around the machine's current tick (2): in the
//! past (tick 1: taken at once, at the current tick), at the current tick,
//! later within a World run, at exactly the World step's limit (8), and
//! past it (10, taken in the next run).  Two arrivals on one IRQ line (at 2
//! and 5) are two deliveries, never merged.  At one tick, callbacks run
//! first, then ISRs, then the tasks the tick woke, each reading that tick.
//! The World steps the machine a bounded number of times: it never
//! busy-wakes it.  See `native_cycle` in sim-ffi for the invariant.

use std::cell::{Cell, RefCell};

use sim_core::{SimConfig, Tick};
use sim_world::firmware::Firmware;
use sim_world::machine::Machine;
use sim_world::world::World;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ran {
    Callback { id: u32, tick: u64 },
    Isr { line: u32, tick: u64 },
    Woke { until: u64, tick: u64 },
}

thread_local! {
    static RAN: RefCell<Vec<Ran>> = const { RefCell::new(Vec::new()) };
    static STEPS: Cell<u32> = const { Cell::new(0) };
}

fn now() -> u64 {
    unsafe { sim_ffi::sim_now_ticks() }
}

fn record(ran: Ran) {
    RAN.with(|r| r.borrow_mut().push(ran));
}

macro_rules! isr {
    ($name:ident, $line:expr) => {
        unsafe extern "C" fn $name() {
            record(Ran::Isr {
                line: $line,
                tick: now(),
            });
        }
    };
}
isr!(isr6, 6);
isr!(isr7, 7);
isr!(isr8, 8);
isr!(isr9, 9);

macro_rules! callback {
    ($name:ident, $id:expr) => {
        unsafe extern "C" fn $name() {
            record(Ran::Callback {
                id: $id,
                tick: now(),
            });
        }
    };
}
callback!(cb_past, 1);
callback!(cb_now, 2);
callback!(cb_later, 4);
callback!(cb_limit, 8);
callback!(cb_task, 100);

fn tick(zephyr: bool) -> u32 {
    unsafe {
        if zephyr {
            sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick()
        } else {
            sim_ffi::sim_scheduler_tick()
        }
    }
}

/// Sleepers' wake-up ticks: now, later, at the limit, past it.
const SLEEPS: [u64; 4] = [2, 3, 8, 10];

struct GridFirmware {
    busy: bool,
    zephyr: bool,
    staged: bool,
}

impl Firmware for GridFirmware {
    fn init(&mut self, machine: &mut Machine) {
        let _active = machine.activate();
        unsafe {
            sim_ffi::device_ffi::sim_irq_set_handler(6, Some(isr6));
            sim_ffi::device_ffi::sim_irq_set_handler(7, Some(isr7));
            sim_ffi::device_ffi::sim_irq_set_handler(8, Some(isr8));
            sim_ffi::device_ffi::sim_irq_set_handler(9, Some(isr9));
        }
        if self.busy {
            sim_ffi::spawn_rust_task("busy", 1, 65536, |ctx| loop {
                ctx.yield_now();
            });
        }
    }

    fn step(&mut self, now: Tick, machine: &mut Machine) {
        STEPS.with(|s| s.set(s.get() + 1));
        let _active = machine.activate();
        tick(self.zephyr);
        if !self.staged && now >= 2_000 {
            self.staged = true;
            assert_eq!(crate::now(), 2, "the machine's clock is not at tick 2");
            sim_devices::irq::with_irq_mut(|c| {
                c.raise_at(7, 1); // past
                c.raise_at(6, 2); // now
                c.raise_at(6, 5); // the same line again, later
                c.raise_at(8, 8); // at the limit
                c.raise_at(9, 10); // past the limit
            });
            unsafe {
                sim_ffi::sim_schedule_event(1, Some(cb_past));
                sim_ffi::sim_schedule_event(2, Some(cb_now));
                sim_ffi::sim_schedule_event(4, Some(cb_later));
                sim_ffi::sim_schedule_event(8, Some(cb_limit));
            }
            for until in SLEEPS {
                // Above the busy task, so each runs as soon as it wakes.
                sim_ffi::spawn_rust_task("sleeper", 5, 65536, move |ctx| {
                    ctx.sleep_until(until);
                    record(Ran::Woke {
                        until,
                        tick: ctx.now(),
                    });
                });
            }
        }
    }
}

fn run(busy: bool, zephyr: bool) -> (Vec<Ran>, Vec<Ran>, u32) {
    std::thread::spawn(move || {
        RAN.with(|r| r.borrow_mut().clear());
        STEPS.with(|s| s.set(0));
        let mut world = World::new();
        world.enable_owned_device_banks();
        let mut machine = Machine::with_defaults(1, "native");
        machine.schedule_at(0, 0, "boot", Box::new(|_| {}));
        // A World event at 2 ms, where the firmware stages its work.
        machine.schedule_at(2_000, 0, "stage", Box::new(|_| {}));
        world.add_machine(machine);
        world
            .machine_mut(1)
            .unwrap()
            .load_firmware(Box::new(GridFirmware {
                busy,
                zephyr,
                staged: false,
            }));
        world.run_until(8_000).unwrap();
        let at_limit = RAN.with(|r| r.borrow().clone());
        world.run_until(12_000).unwrap();
        let all = RAN.with(|r| r.borrow().clone());
        (at_limit, all, STEPS.with(Cell::get))
    })
    .join()
    .unwrap()
}

#[test]
fn every_deadline_runs_once_at_its_own_tick_in_deadline_order() {
    use Ran::{Callback, Isr, Woke};
    let by_limit = vec![
        Callback { id: 1, tick: 2 },
        Callback { id: 2, tick: 2 },
        Isr { line: 6, tick: 2 },
        Isr { line: 7, tick: 2 },
        Woke { until: 2, tick: 2 },
        Woke { until: 3, tick: 3 },
        Callback { id: 4, tick: 4 },
        Isr { line: 6, tick: 5 },
        Callback { id: 8, tick: 8 },
        Isr { line: 8, tick: 8 },
        Woke { until: 8, tick: 8 },
    ];
    let mut all = by_limit.clone();
    all.push(Isr { line: 9, tick: 10 });
    all.push(Woke {
        until: 10,
        tick: 10,
    });
    for busy in [false, true] {
        for zephyr in [false, true] {
            let (at_limit, ran, steps) = run(busy, zephyr);
            let case = format!("busy={busy} zephyr={zephyr}");
            assert_eq!(at_limit, by_limit, "{case}: by the 8 ms limit");
            assert_eq!(ran, all, "{case}: by 12 ms");
            // About one step per firmware tick and per deadline at most
            // (12 ms), plus the World events: never one per µs.
            assert!(steps <= 50, "{case}: {steps} steps in 12 ms");
        }
    }
}

/// A callback a busy task schedules for the current tick runs at once, in
/// the same World step, and the World does not busy-wake the machine.
#[test]
fn a_callback_a_busy_task_schedules_for_now_runs_at_once() {
    struct Firmware2 {
        zephyr: bool,
    }
    impl Firmware for Firmware2 {
        fn init(&mut self, machine: &mut Machine) {
            let _active = machine.activate();
            sim_ffi::spawn_rust_task("busy", 1, 65536, |ctx| {
                unsafe { sim_ffi::sim_schedule_event(ctx.now(), Some(cb_task)) };
                loop {
                    ctx.yield_now();
                }
            });
        }
        fn step(&mut self, now: Tick, machine: &mut Machine) {
            STEPS.with(|s| s.set(s.get() + 1));
            let _active = machine.activate();
            tick(self.zephyr);
            if RAN.with(|r| !r.borrow().is_empty()) {
                CALLBACK_WORLD.with(|c| c.set(c.get().or(Some(now))));
            }
        }
    }
    thread_local! {
        static CALLBACK_WORLD: Cell<Option<Tick>> = const { Cell::new(None) };
    }
    for zephyr in [false, true] {
        let (ran, at, steps) = std::thread::spawn(move || {
            RAN.with(|r| r.borrow_mut().clear());
            STEPS.with(|s| s.set(0));
            let mut world = World::new();
            world.enable_owned_device_banks();
            let mut machine = Machine::with_defaults(1, "native");
            machine.schedule_at(0, 0, "boot", Box::new(|_| {}));
            world.add_machine(machine);
            world
                .machine_mut(1)
                .unwrap()
                .load_firmware(Box::new(Firmware2 { zephyr }));
            world.run_until(2_000).unwrap();
            (
                RAN.with(|r| r.borrow().clone()),
                CALLBACK_WORLD.with(Cell::get),
                STEPS.with(Cell::get),
            )
        })
        .join()
        .unwrap();
        assert_eq!(
            ran,
            vec![Ran::Callback { id: 100, tick: 0 }],
            "zephyr={zephyr}"
        );
        assert!(
            at.is_some_and(|at| at < 1_000),
            "zephyr={zephyr}: ran at {at:?} us"
        );
        assert!(steps <= 10, "zephyr={zephyr}: {steps} steps in 2 ms");
    }
}

/// Without a World: a sleeper due at tick 5, stepped repeatedly under one
/// limit of 10, runs reading tick 5 (time never moves past a deadline
/// whose woken work has not run).
#[test]
fn a_sleeper_wakes_at_its_tick_under_a_later_limit() {
    for busy in [false, true] {
        for zephyr in [false, true] {
            let woke = std::thread::spawn(move || {
                RAN.with(|r| r.borrow_mut().clear());
                let mut sim = sim_ffi::simulator::Simulator::new(SimConfig::default());
                sim.enable_owned_devices();
                let _active = sim.activate();
                sim_ffi::spawn_rust_task("sleeper", 5, 65536, |ctx| {
                    ctx.sleep_until(5);
                    record(Ran::Woke {
                        until: 5,
                        tick: ctx.now(),
                    });
                });
                if busy {
                    sim_ffi::spawn_rust_task("busy", 1, 65536, |ctx| loop {
                        ctx.yield_now();
                    });
                }
                sim.set_scheduler_limit(Some(0));
                tick(zephyr);
                sim.set_scheduler_limit(Some(10));
                for _ in 0..20 {
                    if tick(zephyr) == 0 {
                        break;
                    }
                }
                RAN.with(|r| r.borrow().clone())
            })
            .join()
            .unwrap();
            assert_eq!(
                woke,
                vec![Ran::Woke { until: 5, tick: 5 }],
                "busy={busy} zephyr={zephyr}"
            );
        }
    }
}

/// Without a World: an idle machine at tick 0 with IRQ input due at tick 0,
/// stepped with limit 5, takes it at tick 0 before time moves.
#[test]
fn an_idle_machine_takes_input_due_now_before_time_moves() {
    for zephyr in [false, true] {
        let ran = std::thread::spawn(move || {
            RAN.with(|r| r.borrow_mut().clear());
            let mut sim = sim_ffi::simulator::Simulator::new(SimConfig::default());
            sim.enable_owned_devices();
            let _active = sim.activate();
            sim.set_scheduler_limit(Some(0));
            tick(zephyr);
            unsafe { sim_ffi::device_ffi::sim_irq_set_handler(6, Some(isr6)) };
            sim_devices::irq::with_irq_mut(|c| c.raise_at(6, 0));
            sim.set_scheduler_limit(Some(5));
            // As a World does before the scheduler step.
            sim.catch_up_to_limit();
            tick(zephyr);
            RAN.with(|r| r.borrow().clone())
        })
        .join()
        .unwrap();
        assert_eq!(ran, vec![Ran::Isr { line: 6, tick: 0 }], "zephyr={zephyr}");
    }
}
