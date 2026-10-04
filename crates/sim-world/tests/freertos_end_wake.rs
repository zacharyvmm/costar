//! A machine whose firmware ended the scheduler is not stepped again for
//! a stale wake-up.

use sim_core::Tick;
use sim_world::{firmware::Firmware, machine::Machine, world::World};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
extern "C" {
    fn costar_test_abi_delay_boot();
    fn vTaskEndScheduler();
}
struct Fw {
    steps: Arc<AtomicUsize>,
    ended: bool,
}
impl Firmware for Fw {
    fn init(&mut self, m: &mut Machine) {
        let _a = m.activate();
        unsafe { costar_test_abi_delay_boot() };
    }
    fn step(&mut self, _: Tick, m: &mut Machine) {
        self.steps.fetch_add(1, Ordering::SeqCst);
        let _a = m.activate();
        unsafe { sim_ffi::sim_scheduler_tick() };
        if !self.ended {
            self.ended = true;
            unsafe {
                vTaskEndScheduler();
                sim_ffi::sim_scheduler_tick();
            }
        }
    }
}
#[test]
fn ended_firmware_does_not_schedule_world_steps_forever() {
    let mut w = World::new();
    w.enable_owned_device_banks();
    let steps = Arc::new(AtomicUsize::new(0));
    let mut m = Machine::with_defaults(1, "ecu");
    m.schedule_at(0, 0, "boot", Box::new(|_| {}));
    m.load_firmware(Box::new(Fw {
        steps: steps.clone(),
        ended: false,
    }));
    w.add_machine(m);
    w.run_until(6000).unwrap();
    let n = steps.load(Ordering::SeqCst);
    assert!(n <= 2, "ended firmware was stepped {n} times");
}
