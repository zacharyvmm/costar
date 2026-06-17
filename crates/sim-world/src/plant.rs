//! Environment model trait — external plant/physics models that step
//! each tick alongside the World.
//!
//! A [`EnvironmentModel`] implementation models the physical environment
//! (vehicle dynamics, battery chemistry, thermal systems, etc.) outside
//! the firmware domain.  The World calls [`step`](EnvironmentModel::step)
//! periodically, giving the model read/write access to buses and machines.

use sim_core::Tick;

use crate::world::World;

/// An external environment model that advances in lockstep with the
/// simulation World.
///
/// Implementations receive a mutable reference to the [`World`] so they
/// can publish sensor readings onto buses, read actuator commands, and
/// schedule events.
///
/// # Example (microcar plant)
///
/// ```ignore
/// struct VehiclePlant { ... }
///
/// impl EnvironmentModel for VehiclePlant {
///     fn step(&mut self, now: Tick, world: &mut World) {
///         // Read motor torque command from the powertrain on vcan0
///         // Update speed and battery state
///         // Publish wheel speed and BMS sensor readings onto vcan0
///     }
/// }
/// ```
pub trait EnvironmentModel {
    /// Advance the environment model by one tick.
    ///
    /// `now` is the current virtual time in ticks (1 tick = 1 µs in
    /// the bus/plant convention used by scenario files).
    ///
    /// `world` provides mutable access to machines, links, and buses
    /// so the model can publish frames and read actuator state.
    fn step(&mut self, now: Tick, world: &mut World);

    /// Queue a driver input to be applied at a specific virtual time.
    ///
    /// The model should store this and apply it during the next
    /// [`step`](EnvironmentModel::step) call where `now >= at`.
    fn queue_driver_input(&mut self, at: Tick, throttle_percent: u8, brake_pressed: bool);

    /// Apply a fault injection targeted at a plant subcomponent.
    ///
    /// `target` is the subcomponent name (e.g., "battery").
    /// `fault_type` is the fault type (e.g., "force_temperature").
    /// `value` is an optional numeric value (e.g., temperature in °C).
    ///
    /// Returns `true` if the fault was recognised and applied.
    fn apply_fault(&mut self, target: &str, fault_type: &str, value: Option<u32>) -> bool {
        let _ = (target, fault_type, value);
        false
    }
}
