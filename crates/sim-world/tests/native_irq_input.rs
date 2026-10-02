//! World IRQ input (`Machine::raise_irq`) reaches firmware that is not
//! scheduled by FreeRTOS: the native scheduler (also behind the Zephyr
//! scheduler step) treats scheduled IRQ arrivals as deadlines, the World
//! wakes the machine for them, and the ISR runs at the arrival — not
//! before, and not never — and each ISR reads its arrival tick.  A World
//! step never runs native work past its limit.  The Zephyr scheduler loop
//! takes such input too, with or without threads.

use std::cell::RefCell;

use sim_core::{SimConfig, Tick};
use sim_world::firmware::Firmware;
use sim_world::machine::Machine;
use sim_world::world::World;

thread_local! {
    /// Firmware tick of every ISR run.
    static ISR_AT: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "C" fn isr() {
    ISR_AT.with(|a| a.borrow_mut().push(sim_ffi::sim_now_ticks()));
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    /// No task at all: the machine is idle from tick 0.
    Idle,
    /// A task sleeping past the arrival.
    Sleeper,
}

struct NativeFirmware {
    kind: Kind,
    zephyr: bool,
}

impl Firmware for NativeFirmware {
    fn init(&mut self, machine: &mut Machine) {
        let _active = machine.activate();
        unsafe { sim_ffi::device_ffi::sim_irq_set_handler(6, Some(isr)) };
        if let Kind::Sleeper = self.kind {
            sim_ffi::spawn_rust_task("sleeper", 1, 4096, |ctx| ctx.sleep_until(9));
        }
    }

    fn step(&mut self, _now: Tick, machine: &mut Machine) {
        let _active = machine.activate();
        unsafe {
            if self.zephyr {
                sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick();
            } else {
                sim_ffi::sim_scheduler_tick();
            }
        }
    }
}

fn isr_ticks(kind: Kind, zephyr: bool, staged_after_boot: bool) -> Vec<u64> {
    ISR_AT.with(|a| a.borrow_mut().clear());
    let mut world = World::new();
    world.enable_owned_device_banks();
    let mut machine = Machine::with_defaults(1, "native");
    machine.schedule_at(0, 0, "boot", Box::new(|_| {}));
    world.add_machine(machine);
    world
        .machine_mut(1)
        .unwrap()
        .load_firmware(Box::new(NativeFirmware { kind, zephyr }));
    if staged_after_boot {
        // The machine has booted and gone idle at tick 0.
        world.run_until(1_000).unwrap();
    }
    // IRQ 6 arrives at 5 ms (firmware tick 5).
    world.machine_mut(1).unwrap().raise_irq(6, 5_000);
    world.run_until(10_000).unwrap();
    ISR_AT.with(|a| a.borrow().clone())
}

#[test]
fn world_irq_input_reaches_native_firmware_at_its_arrival() {
    for kind in [Kind::Idle, Kind::Sleeper] {
        for zephyr in [false, true] {
            for staged_after_boot in [false, true] {
                // Each case on a thread of its own: the Zephyr scheduler
                // step keeps its clock in thread-local state.
                let ticks = std::thread::spawn(move || isr_ticks(kind, zephyr, staged_after_boot))
                    .join()
                    .unwrap();
                assert_eq!(
                    ticks,
                    vec![5],
                    "{kind:?} zephyr={zephyr} staged_after_boot={staged_after_boot}"
                );
            }
        }
    }
}

unsafe extern "C" fn zephyr_sleeper(
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
) {
    sim_ffi::sim_task_delay_until(9);
}

/// The Zephyr scheduler loop takes scheduled IRQ input at its arrival.
#[test]
fn zephyr_scheduler_loop_takes_scheduled_irq_input() {
    ISR_AT.with(|a| a.borrow_mut().clear());
    let mut sim = sim_ffi::simulator::Simulator::new(SimConfig::default());
    sim.enable_owned_devices();
    let _a = sim.activate();
    unsafe {
        sim_ffi::device_ffi::sim_irq_set_handler(6, Some(isr));
        sim_ffi::zephyr_ffi::sim_zephyr_register_thread(
            c"sleeper".as_ptr(),
            Some(zephyr_sleeper),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            4096,
            1,
        );
    }
    sim_devices::irq::with_irq_mut(|c| c.raise_at(6, 5));
    unsafe { sim_ffi::zephyr_ffi::sim_zephyr_start_scheduler() };
    assert_eq!(ISR_AT.with(|a| a.borrow().clone()), vec![5]);
}

/// A native task sleeps until tick 9; IRQ input arrives at 5 ms and 7 ms.
/// A World step never runs work past its limit: running the World to 5 ms
/// takes only the first IRQ, and each ISR reads its arrival tick.
#[test]
fn native_irq_input_is_never_taken_past_the_world_step() {
    let run = || {
        ISR_AT.with(|a| a.borrow_mut().clear());
        let mut world = World::new();
        world.enable_owned_device_banks();
        let mut machine = Machine::with_defaults(1, "native");
        machine.schedule_at(0, 0, "boot", Box::new(|_| {}));
        world.add_machine(machine);
        world
            .machine_mut(1)
            .unwrap()
            .load_firmware(Box::new(NativeFirmware {
                kind: Kind::Sleeper,
                zephyr: false,
            }));
        world.machine_mut(1).unwrap().raise_irq(6, 5_000);
        world.machine_mut(1).unwrap().raise_irq(6, 7_000);
        world.run_until(5_000).unwrap();
        let at_5ms = ISR_AT.with(|a| a.borrow().clone());
        world.run_until(10_000).unwrap();
        let at_10ms = ISR_AT.with(|a| a.borrow().clone());
        (at_5ms, at_10ms)
    };
    let (at_5ms, at_10ms) = std::thread::spawn(run).join().unwrap();
    assert_eq!(at_5ms, vec![5]);
    assert_eq!(at_10ms, vec![5, 7]);
}

/// The Zephyr scheduler loop takes scheduled IRQ input even with no thread
/// at all, and returns once nothing is left.
#[test]
fn zephyr_scheduler_loop_takes_irq_input_without_threads() {
    ISR_AT.with(|a| a.borrow_mut().clear());
    let mut sim = sim_ffi::simulator::Simulator::new(SimConfig::default());
    sim.enable_owned_devices();
    let _a = sim.activate();
    unsafe { sim_ffi::device_ffi::sim_irq_set_handler(6, Some(isr)) };
    sim_devices::irq::with_irq_mut(|c| c.raise_at(6, 5));
    sim_devices::irq::with_irq_mut(|c| c.raise_at(6, 8));
    unsafe { sim_ffi::zephyr_ffi::sim_zephyr_start_scheduler() };
    assert_eq!(ISR_AT.with(|a| a.borrow().clone()), vec![5, 8]);
}
