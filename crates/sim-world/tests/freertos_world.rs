//! FreeRTOS firmware inside a World.
//!
//! Two machines run the same FreeRTOS firmware.  Each gets its own kernel,
//! idle task and timer service, and firmware time follows the World clock:
//! a machine never runs ahead of the World.

use sim_core::Tick;
use sim_world::firmware::Firmware;
use sim_world::machine::Machine;
use sim_world::scenario::Scenario;
use sim_world::world::World;

extern "C" {
    fn costar_test_software_timer_boot();
    fn costar_test_external_irq_boot();
}

/// Boots the periodic-timer scenario (a 10-tick auto-reload timer).
struct TimerFirmware;

impl Firmware for TimerFirmware {
    fn init(&mut self, machine: &mut Machine) {
        let _active = machine.activate();
        unsafe { costar_test_software_timer_boot() };
    }

    fn step(&mut self, _now: Tick, machine: &mut Machine) {
        let _active = machine.activate();
        unsafe { sim_ffi::sim_scheduler_tick() };
        sim_ffi::flush_trace();
    }
}

/// Boots a task that waits for IRQ 6.  Like a device model, the host side
/// raises the IRQ at World time `irq_at`, before stepping the firmware.
struct IrqWaiterFirmware {
    irq_at: Option<Tick>,
}

impl Firmware for IrqWaiterFirmware {
    fn init(&mut self, machine: &mut Machine) {
        let _active = machine.activate();
        unsafe { costar_test_external_irq_boot() };
    }

    fn step(&mut self, now: Tick, machine: &mut Machine) {
        let _active = machine.activate();
        if self.irq_at.is_some_and(|at| now >= at) {
            self.irq_at = None;
            sim_devices::irq::with_irq_mut(|c| c.raise(6));
        }
        unsafe { sim_ffi::sim_scheduler_tick() };
        sim_ffi::flush_trace();
    }
}

/// World-time (µs) timestamps of `label` records for one machine.
fn record_times(world: &World, machine: u64, label: &str) -> Vec<u64> {
    let prefix = format!("[machine.{machine}]");
    let label = format!("\"{label}\"");
    world
        .drain_all_traces()
        .iter()
        .filter(|line| line.starts_with(&prefix) && line.contains(&label))
        .map(|line| {
            line[prefix.len()..]
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap()
        })
        .collect()
}

/// World-time (µs) timestamps of `timer_fired` records for one machine.
fn timer_fires(world: &World, machine: u64) -> Vec<u64> {
    record_times(world, machine, "timer_fired")
}

#[test]
fn two_machines_run_their_own_freertos_in_step_with_world_time() {
    let mut world = World::new();
    for id in 1..=2 {
        let mut machine = Machine::with_defaults(id, &format!("m{id}"));
        // Kick the first World step at t=0 so the firmware boots.
        machine.schedule_at(0, 0, "boot", Box::new(|_| {}));
        machine.load_firmware(Box::new(TimerFirmware));
        world.add_machine(machine);
    }

    // World time is in microseconds; FreeRTOS ticks are 1 ms.
    world.run_until(55_000).unwrap();

    for id in 1..=2 {
        assert_eq!(
            timer_fires(&world, id),
            vec![10_000, 20_000, 30_000, 40_000, 50_000],
            "machine {id}"
        );
    }

    // Continuing picks up exactly where the World clock left off.
    world.run_until(75_000).unwrap();
    for id in 1..=2 {
        assert_eq!(
            timer_fires(&world, id),
            vec![10_000, 20_000, 30_000, 40_000, 50_000, 60_000, 70_000],
            "machine {id}"
        );
    }
}

/// `before_ms` deadlines are checked against firmware events in World time:
/// the timer first fires at tick 10, which is 10 ms (not 10 µs).
#[test]
fn scenario_deadlines_apply_to_firmware_events_in_world_time() {
    let mut world = World::new();
    let mut machine = Machine::with_defaults(1, "ecu");
    machine.schedule_at(0, 0, "boot", Box::new(|_| {}));
    machine.load_firmware(Box::new(TimerFirmware));
    world.add_machine(machine);
    world.run_until(15_000).unwrap();
    let trace = world.drain_all_traces();

    let check = |kind: &str, before_ms: u64| {
        let toml = format!(
            "name = \"t\"\n[[machine]]\nid = 1\nname = \"ecu\"\n\
             [expect]\n[[expect.{kind}]]\nbefore_ms = {before_ms}\n\
             machine = \"ecu\"\nevent = \"timer_fired\"\n"
        );
        Scenario::from_str(&toml)
            .unwrap()
            .check_trace(trace.clone())
            .unwrap()
            .trace_match
    };

    assert!(!check("event", 9), "fired at 10 ms, not before 9 ms");
    assert!(check("event", 11), "fired at 10 ms, before 11 ms");
    assert!(check("no", 9), "nothing fired before 9 ms");
    assert!(!check("no", 11), "fired at 10 ms, before 11 ms");
}

#[test]
fn external_interrupt_runs_at_world_time_on_an_idle_machine() {
    let mut world = World::new();
    let mut machine = Machine::with_defaults(1, "m1");
    // Kick the first World step at t=0 so the firmware boots.
    machine.schedule_at(0, 0, "boot", Box::new(|_| {}));
    // Input from outside the firmware at 5 ms, while every task is blocked
    // (the event only makes the World step the machine then).
    machine.schedule_at(5_000, 0, "input", Box::new(|_| {}));
    machine.load_firmware(Box::new(IrqWaiterFirmware {
        irq_at: Some(5_000),
    }));
    world.add_machine(machine);

    world.run_until(10_000).unwrap();

    assert_eq!(record_times(&world, 1, "timer_isr"), vec![5_000]);
    assert_eq!(record_times(&world, 1, "isr_woke_task"), vec![5_000]);
}
