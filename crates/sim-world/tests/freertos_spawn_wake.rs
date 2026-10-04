//! A native task spawned by `Firmware::step` after the firmware went
//! quiescent still runs: the World steps the machine again.

use sim_core::Tick;
use sim_world::{firmware::Firmware, machine::Machine, world::World};
extern "C" {
    fn costar_test_abi_delay_boot();
}
struct SpawningFirmware {
    spawned: bool,
}
impl Firmware for SpawningFirmware {
    fn init(&mut self, machine: &mut Machine) {
        let _active = machine.activate();
        unsafe {
            costar_test_abi_delay_boot();
        }
    }
    fn step(&mut self, now: Tick, machine: &mut Machine) {
        let _active = machine.activate();
        let more = unsafe { sim_ffi::sim_scheduler_tick() };
        if more == 0 && !self.spawned {
            assert_eq!(now, 12_000);
            self.spawned = true;
            sim_ffi::spawn_rust_task("new", 5, 4096, |_| unsafe {
                sim_ffi::sim_trace_u32(c"new_native_ran".as_ptr(), 1);
            });
        }
    }
}
#[test]
fn native_spawn_after_quiescent_step_keeps_world_running() {
    let mut world = World::new();
    let mut machine = Machine::with_defaults(1, "ecu");
    machine.schedule_at(0, 0, "boot", Box::new(|_| {}));
    machine.load_firmware(Box::new(SpawningFirmware { spawned: false }));
    world.add_machine(machine);
    world.run_until(20_000).unwrap();
    let records: Vec<_> = world
        .drain_all_traces()
        .into_iter()
        .filter(|l| l.contains("\"new_native_ran\""))
        .collect();
    assert_eq!(records.len(), 1, "World dropped the pending native task");
}
