//! World IRQ input reaches native firmware that is never idle.
//!
//! A native (or Zephyr-step) machine whose task stays runnable — busy
//! yielding, or yielding until its ISR sets a flag — still sees time pass:
//! each World step brings its firmware clock up to the step's limit before
//! the task resumes, taking IRQ input on the way.  So an IRQ staged for
//! 5 ms runs at World time 5 ms, reading firmware tick 5, and a cooperative
//! wait on it completes.  The World does not busy-wake such a machine: it
//! steps it at most once per firmware tick when nothing else happens.

use std::cell::{Cell, RefCell};

use sim_core::Tick;
use sim_world::firmware::Firmware;
use sim_world::machine::Machine;
use sim_world::world::World;

thread_local! {
    /// Firmware tick of every ISR run.
    static ISR_TICKS: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    /// World time of the firmware step in which each ISR ran.
    static ISR_WORLD: RefCell<Vec<Tick>> = const { RefCell::new(Vec::new()) };
    static FLAG: Cell<bool> = const { Cell::new(false) };
    /// Firmware tick at which the cooperative waiter saw the flag.
    static DONE_AT: Cell<Option<u64>> = const { Cell::new(None) };
    static YIELDS: Cell<u64> = const { Cell::new(0) };
    static STEPS: Cell<u64> = const { Cell::new(0) };
}

unsafe extern "C" fn isr() {
    ISR_TICKS.with(|a| a.borrow_mut().push(sim_ffi::sim_now_ticks()));
    FLAG.with(|f| f.set(true));
}

#[derive(Clone, Copy, Debug)]
enum Task {
    /// Yields a fixed number of times, whatever happens.
    BusyYield,
    /// Yields until its ISR sets a flag.
    WaitForIsr,
}

struct BusyFirmware {
    task: Task,
    zephyr: bool,
}

impl Firmware for BusyFirmware {
    fn init(&mut self, machine: &mut Machine) {
        let _active = machine.activate();
        unsafe { sim_ffi::device_ffi::sim_irq_set_handler(6, Some(isr)) };
        match self.task {
            Task::BusyYield => {
                sim_ffi::spawn_rust_task("busy", 1, 65536, |ctx| {
                    for _ in 0..7000 {
                        YIELDS.with(|y| y.set(y.get() + 1));
                        ctx.yield_now();
                    }
                });
            }
            Task::WaitForIsr => {
                sim_ffi::spawn_rust_task("waiter", 1, 65536, |ctx| {
                    while !FLAG.with(Cell::get) {
                        YIELDS.with(|y| y.set(y.get() + 1));
                        ctx.yield_now();
                    }
                    DONE_AT.with(|d| d.set(Some(ctx.now())));
                });
            }
        }
    }

    fn step(&mut self, now: Tick, machine: &mut Machine) {
        STEPS.with(|s| s.set(s.get() + 1));
        {
            let _active = machine.activate();
            unsafe {
                if self.zephyr {
                    sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick();
                } else {
                    sim_ffi::sim_scheduler_tick();
                }
            }
        }
        // Every ISR since the last step ran in this one (in
        // `begin_firmware_step`, which takes input due by `now`, or in the
        // scheduler).
        let total = ISR_TICKS.with(|a| a.borrow().len());
        let seen = ISR_WORLD.with(|w| w.borrow().len());
        for _ in seen..total {
            ISR_WORLD.with(|w| w.borrow_mut().push(now));
        }
    }
}

struct Outcome {
    isr_ticks: Vec<u64>,
    isr_world: Vec<Tick>,
    done_at: Option<u64>,
    yields: u64,
    steps: u64,
}

fn run(task: Task, zephyr: bool) -> Outcome {
    std::thread::spawn(move || {
        let mut world = World::new();
        world.enable_owned_device_banks();
        let mut machine = Machine::with_defaults(1, "native");
        machine.schedule_at(0, 0, "boot", Box::new(|_| {}));
        world.add_machine(machine);
        world
            .machine_mut(1)
            .unwrap()
            .load_firmware(Box::new(BusyFirmware { task, zephyr }));
        // IRQ 6 arrives at 5 ms (firmware tick 5).
        world.machine_mut(1).unwrap().raise_irq(6, 5_000);
        world.run_until(10_000).unwrap();
        Outcome {
            isr_ticks: ISR_TICKS.with(|a| a.borrow().clone()),
            isr_world: ISR_WORLD.with(|a| a.borrow().clone()),
            done_at: DONE_AT.with(Cell::get),
            yields: YIELDS.with(Cell::get),
            steps: STEPS.with(Cell::get),
        }
    })
    .join()
    .unwrap()
}

#[test]
fn a_busy_yielding_native_task_does_not_hold_off_world_irq_input() {
    for zephyr in [false, true] {
        let out = run(Task::BusyYield, zephyr);
        assert_eq!(out.isr_ticks, vec![5], "zephyr={zephyr}");
        assert_eq!(out.isr_world, vec![5_000], "zephyr={zephyr}");
        // The task kept running, and the World stepped the machine about
        // once per firmware tick, not once per microsecond.
        assert!(out.yields >= 10, "zephyr={zephyr}: {} yields", out.yields);
        assert!(out.steps <= 30, "zephyr={zephyr}: {} steps", out.steps);
    }
}

#[test]
fn a_cooperative_wait_on_an_isr_completes_under_a_world() {
    for zephyr in [false, true] {
        let out = run(Task::WaitForIsr, zephyr);
        assert_eq!(out.isr_ticks, vec![5], "zephyr={zephyr}");
        assert_eq!(out.isr_world, vec![5_000], "zephyr={zephyr}");
        assert_eq!(out.done_at, Some(5), "zephyr={zephyr}");
        assert!(out.steps <= 30, "zephyr={zephyr}: {} steps", out.steps);
    }
}

/// The same without a World: a caller stepping a busy native machine with
/// a rising scheduler limit (one step per limit) still takes IRQ input at
/// its arrival tick.
#[test]
fn bounded_native_steps_take_irq_input_while_a_task_is_busy() {
    for zephyr in [false, true] {
        let ticks = std::thread::spawn(move || {
            let mut sim = sim_ffi::simulator::Simulator::new(sim_core::SimConfig::default());
            sim.enable_owned_devices();
            let _active = sim.activate();
            unsafe { sim_ffi::device_ffi::sim_irq_set_handler(6, Some(isr)) };
            sim_ffi::spawn_rust_task("busy", 1, 65536, |ctx| loop {
                ctx.yield_now();
            });
            sim_devices::irq::with_irq_mut(|c| c.raise_at(6, 5));
            for limit in 0..10 {
                sim.set_scheduler_limit(Some(limit));
                unsafe {
                    if zephyr {
                        sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick();
                    } else {
                        sim_ffi::sim_scheduler_tick();
                    }
                }
            }
            ISR_TICKS.with(|a| a.borrow().clone())
        })
        .join()
        .unwrap();
        assert_eq!(ticks, vec![5], "zephyr={zephyr}");
    }
}
