//! Firmware loaded onto a machine boots at once, but its pending boot is
//! only one more event at the World's current time: an earlier event
//! queued on the machine still comes first, so it still meets the World's
//! backward-time check, with or without firmware.

use std::sync::{Arc, Mutex};

use sim_world::{Firmware, Machine, World};

struct LateFirmware(Arc<Mutex<Vec<u64>>>);

impl Firmware for LateFirmware {
    fn step(&mut self, now: u64, _: &mut Machine) {
        self.0.lock().unwrap().push(now);
    }
}

/// The World at t=100 gains a machine with an event queued at t=50.
/// Returns the step's result, the firmware steps and the dispatched events.
fn stale_event_step(
    load_firmware: bool,
) -> (
    Result<sim_world::StepOutcome, sim_core::SimError>,
    Vec<u64>,
    Vec<u64>,
) {
    let mut world = World::new();
    world.enable_owned_device_banks();
    let mut clock = Machine::with_defaults(1, "clock");
    clock.schedule_at(100, 0, "advance", Box::new(|_| {}));
    world.add_machine(clock);
    world.run_until(100).unwrap();
    assert_eq!(world.now, 100);

    let fired = Arc::new(Mutex::new(Vec::new()));
    let observed = fired.clone();
    let mut late = Machine::with_defaults(2, "late");
    late.schedule_at(
        50,
        0,
        "past",
        Box::new(move |_| observed.lock().unwrap().push(50u64)),
    );
    world.add_machine(late);
    let steps = Arc::new(Mutex::new(Vec::new()));
    if load_firmware {
        world
            .machine_mut(2)
            .unwrap()
            .load_firmware(Box::new(LateFirmware(steps.clone())));
    }
    let result = world.step();
    let steps = steps.lock().unwrap().clone();
    let fired = fired.lock().unwrap().clone();
    (result, steps, fired)
}

#[test]
fn a_stale_event_is_rejected_without_firmware() {
    let (result, steps, fired) = stale_event_step(false);
    assert!(
        matches!(
            result,
            Err(sim_core::SimError::TimeWentBackwards {
                now: 100,
                event_at: 50
            })
        ),
        "{result:?}"
    );
    assert!(steps.is_empty() && fired.is_empty());
}

#[test]
fn a_pending_boot_does_not_hide_a_stale_event() {
    let (result, steps, fired) = stale_event_step(true);
    assert!(
        matches!(
            result,
            Err(sim_core::SimError::TimeWentBackwards {
                now: 100,
                event_at: 50
            })
        ),
        "{result:?}"
    );
    // Neither the firmware nor the stale callback ran.
    assert!(steps.is_empty(), "firmware stepped: {steps:?}");
    assert!(fired.is_empty(), "stale callback dispatched");
}
