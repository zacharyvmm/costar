//! FreeRTOS firmware inside a World.
//!
//! Two machines run the same FreeRTOS firmware.  Each gets its own kernel,
//! idle task and timer service, and firmware time follows the World clock:
//! a machine never runs ahead of the World.

use sim_core::Tick;
use sim_world::firmware::Firmware;
use sim_world::machine::Machine;
use sim_world::world::World;

extern "C" {
    fn costar_test_software_timer_boot();
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

/// Firmware-tick timestamps of `timer_fired` records for one machine.
fn timer_fires(world: &World, machine: u64) -> Vec<u64> {
    let prefix = format!("[machine.{machine}]");
    world
        .drain_all_traces()
        .iter()
        .filter(|line| line.starts_with(&prefix) && line.contains("\"timer_fired\""))
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

#[test]
fn two_machines_run_their_own_freertos_in_step_with_world_time() {
    let mut world = World::new();
    for id in 1..=2 {
        let mut machine = Machine::with_defaults(id, &format!("m{id}"));
        machine.load_firmware(Box::new(TimerFirmware));
        world.add_machine(machine);
    }

    // World time is in microseconds; FreeRTOS ticks are 1 ms.
    world.run_until(55_000).unwrap();

    for id in 1..=2 {
        assert_eq!(
            timer_fires(&world, id),
            vec![10, 20, 30, 40, 50],
            "machine {id}"
        );
    }

    // Continuing picks up exactly where the World clock left off.
    world.run_until(75_000).unwrap();
    for id in 1..=2 {
        assert_eq!(
            timer_fires(&world, id),
            vec![10, 20, 30, 40, 50, 60, 70],
            "machine {id}"
        );
    }
}
