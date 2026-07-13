//! Firmware trait — guest firmware loaded onto simulated machines.
//!
//! A [`Firmware`] implementation represents the application logic running
//! on a simulated ECU.  The [`World`](super::World) calls lifecycle methods
//! on each machine's firmware during the simulation run loop.
//!
//! Firmware is the dual of [`EnvironmentModel`](super::EnvironmentModel):
//! the environment model represents the physical world (plant, physics),
//! while firmware represents the control logic running on each ECU.
//!
//! # Example (minimal firmware)
//!
//! ```ignore
//! struct MyFirmware;
//!
//! impl Firmware for MyFirmware {
//!     fn init(&mut self, machine: &mut Machine) {
//!         // Spawn a periodic task.
//!         machine.schedule_at(0, 0, "init", Box::new(|ctx| {
//!             // task body
//!         }));
//!     }
//! }
//! ```

use sim_core::Tick;

use crate::machine::Machine;

/// A cloneable factory that constructs a fresh instance of a machine's guest
/// firmware.  Stored on a [`Machine`] so a restart
/// ([`FaultAction::Reboot`](crate::world::FaultAction::Reboot)) can recreate the
/// *original* firmware and run its normal boot path, instead of leaving a bare
/// machine.  It is an `Arc` so it survives replacing the `Machine` on restart.
pub type FirmwareFactory = std::sync::Arc<dyn Fn() -> Box<dyn Firmware>>;

/// Guest firmware loaded onto a simulated machine.
///
/// Implementations receive a mutable reference to their host [`Machine`]
/// so they can spawn tasks, schedule events, and record trace events.
///
/// # Lifecycle
///
/// 1. [`init`](Firmware::init) — called once when the firmware is attached
///    to a machine (via [`Machine::load_firmware`]).
/// 2. [`step`](Firmware::step) — called each tick of the World's run loop,
///    after faults are applied and before machine events are dispatched.
///    This is where the firmware can react to incoming messages and
///    schedule new work.
pub trait Firmware {
    /// Called once when the firmware is attached to a machine.
    ///
    /// Use this to spawn initial tasks, schedule startup events, and
    /// configure the machine.
    fn init(&mut self, _machine: &mut Machine) {}

    /// Called each simulation tick after faults are applied, but before
    /// machine events are dispatched.
    ///
    /// `now` is the current virtual time in ticks.
    /// `machine` is the host machine — use it to schedule events, spawn
    /// tasks, and inspect trace output.
    fn step(&mut self, now: Tick, machine: &mut Machine) {
        let _ = (now, machine);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::Machine;
    use sim_core::SimConfig;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;

    /// A firmware that records init/step calls.
    struct RecordingFirmware {
        init_called: Arc<AtomicBool>,
        step_count: Arc<AtomicU32>,
    }

    impl Firmware for RecordingFirmware {
        fn init(&mut self, _machine: &mut Machine) {
            self.init_called.store(true, Ordering::SeqCst);
        }

        fn step(&mut self, _now: Tick, _machine: &mut Machine) {
            self.step_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn test_firmware_init_called_on_load() {
        let init_called = Arc::new(AtomicBool::new(false));
        let step_count = Arc::new(AtomicU32::new(0));

        let fw = RecordingFirmware {
            init_called: init_called.clone(),
            step_count: step_count.clone(),
        };

        let mut machine = Machine::with_defaults(0, "test");
        assert!(!init_called.load(Ordering::SeqCst));

        machine.load_firmware(Box::new(fw));
        assert!(init_called.load(Ordering::SeqCst));
        assert!(machine.has_firmware());
    }

    #[test]
    fn test_firmware_step_called() {
        let init_called = Arc::new(AtomicBool::new(false));
        let step_count = Arc::new(AtomicU32::new(0));

        let fw = RecordingFirmware {
            init_called: init_called.clone(),
            step_count: step_count.clone(),
        };

        let mut machine = Machine::with_defaults(0, "test");
        machine.load_firmware(Box::new(fw));

        // Simulate stepping firmware manually.
        let mut fw = machine.take_firmware().unwrap();
        fw.step(100, &mut machine);
        machine.set_firmware(fw);

        assert_eq!(step_count.load(Ordering::SeqCst), 1);
    }

    /// A firmware that schedules an event at init and records steps.
    struct SchedulingFirmware {
        step_ticks: Vec<Tick>,
    }

    impl Firmware for SchedulingFirmware {
        fn init(&mut self, machine: &mut Machine) {
            // Schedule a task to fire at tick 50.
            machine.schedule_at(50, 0, "fw-task", Box::new(|_ctx| {}));
        }

        fn step(&mut self, now: Tick, _machine: &mut Machine) {
            self.step_ticks.push(now);
        }
    }

    #[test]
    fn test_firmware_schedules_events() {
        let fw = SchedulingFirmware {
            step_ticks: Vec::new(),
        };

        let mut machine = Machine::new(0, "test", SimConfig::default());
        machine.load_firmware(Box::new(fw));

        // Verify event was scheduled by firmware init.
        assert_eq!(machine.next_event_time(), Some(50));
    }

    #[test]
    fn test_firmware_take_and_set() {
        let fw = RecordingFirmware {
            init_called: Arc::new(AtomicBool::new(false)),
            step_count: Arc::new(AtomicU32::new(0)),
        };

        let mut machine = Machine::with_defaults(0, "test");
        machine.load_firmware(Box::new(fw));
        assert!(machine.has_firmware());

        let taken = machine.take_firmware();
        assert!(taken.is_some());
        assert!(!machine.has_firmware());

        machine.set_firmware(taken.unwrap());
        assert!(machine.has_firmware());
    }
}
