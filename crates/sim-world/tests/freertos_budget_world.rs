//! A budget exhausted at a World step's limit is not resumed by another
//! World step within the same firmware tick.

use sim_core::Tick;
use sim_world::{firmware::Firmware, machine::Machine, world::World};
extern "C" {
    fn costar_test_abi_delay_boot();
}
struct BudgetFirmware;
impl Firmware for BudgetFirmware {
    fn init(&mut self, machine: &mut Machine) {
        let _active = machine.activate();
        unsafe {
            costar_test_abi_delay_boot();
        }
        sim_ffi::spawn_rust_task("high", 6, 4096, |ctx| {
            ctx.sleep_for(1);
        });
        sim_ffi::spawn_rust_task("low", 5, 4096, |ctx| unsafe {
            sim_ffi::sim_budget_set_limit(1);
            sim_ffi::sim_budget_poll(std::ptr::null(), 1);
            sim_ffi::sim_trace_u32(c"after_budget".as_ptr(), ctx.now() as u32);
        });
    }
    fn step(&mut self, _: Tick, machine: &mut Machine) {
        let _active = machine.activate();
        unsafe {
            sim_ffi::sim_scheduler_tick();
        }
    }
}
#[test]
fn world_event_at_500us_does_not_resume_exhausted_work_at_tick_zero() {
    let mut world = World::new();
    let mut machine = Machine::with_defaults(1, "ecu");
    machine.schedule_at(0, 0, "boot", Box::new(|_| {}));
    machine.schedule_at(500, 0, "another_world_event", Box::new(|_| {}));
    machine.load_firmware(Box::new(BudgetFirmware));
    world.add_machine(machine);
    world.run_until(2_000).unwrap();
    let lines: Vec<_> = world
        .drain_all_traces()
        .into_iter()
        .filter(|l| l.contains("\"after_budget\""))
        .collect();
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].split_whitespace().nth(1) == Some("1000"),
        "budget work resumed too early: {lines:?}"
    );
}
