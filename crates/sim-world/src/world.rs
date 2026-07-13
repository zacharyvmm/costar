//! Global event loop coordinating multiple simulated machines.
//!
//! A [`World`] owns a collection of [`Machine`]s and [`Link`]s.  It
//! advances virtual time to the earliest deadline across all machines
//! and links, delivering link packets and dispatching machine events
//! at the right virtual time.
//!
//! All machines share the same virtual clock — events from different
//! machines are interleaved deterministically by timestamp, priority,
//! and machine ID.

use std::collections::BTreeMap;

use sim_core::{SimError, Tick, TraceEvent};
use sim_devices::CanFrame;
use sim_net;

use crate::canbus::CanBus;
use crate::firmware::Firmware;
use crate::link::Link;
use crate::machine::Machine;
use crate::plant::EnvironmentModel;
use crate::predicate::{ContinuePredicate, ScalarValue, SemanticEvent};

use crate::board::BoardConfig;
use crate::firmware::FirmwareFactory;

/// Immutable machine specification preserved across a Stage A3 restart.
///
/// Stored when a machine is removed for reboot and used to reconstruct it
/// at the end of the downtime window with the same identity, RTOS, firmware
/// factory, board config, and `SimConfig`.
#[derive(Clone)]
struct RestartSpec {
    name: String,
    rtos: crate::RtosBackend,
    firmware_factory: Option<FirmwareFactory>,
    board: BoardConfig,
    config: sim_core::SimConfig,
}

/// A fault action scheduled at a specific virtual time.
///
/// Faults are injected during the World's run loop at their scheduled
/// time.  They modify the simulation state: changing plant parameters,
/// pausing machine heartbeats, rebooting machines, or altering bus
/// behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaultAction {
    /// Force a plant subcomponent parameter (e.g., battery temperature).
    ForceTemperature {
        /// Target subcomponent (e.g., "battery").
        target: String,
        /// Temperature in Celsius.
        value_c: u32,
    },
    /// Pause a machine (stop its heartbeat / make it unresponsive).
    StopHeartbeat {
        /// Machine ID to pause.
        machine_id: u64,
    },
    /// Reboot a machine using the Stage A3 restart algorithm: snapshot persistent
    /// devices, destroy old machine, wait for `downtime_ms`, then reconstruct
    /// from immutable spec + persistent state.  Emits `machine_reset_begin` at
    /// fault time and `machine_reset_boot` after reconstruction.
    Reboot {
        /// Machine ID to reboot.
        machine_id: u64,
        /// Downtime in virtual-time microseconds before the machine boots again.
        /// Frames sent to this machine during downtime are dropped.
        downtime_ms: u64,
    },
    /// Drop all frames with a specific CAN ID on a bus.
    DropFrame {
        /// Bus name.
        bus_name: String,
        /// CAN frame ID to drop.
        frame_id: u32,
    },
    /// Add extra delivery latency to frames with a specific CAN ID on a bus.
    DelayFrame {
        /// Bus name.
        bus_name: String,
        /// CAN frame ID to delay.
        frame_id: u32,
        /// Extra delay in virtual-time ticks (microseconds).
        delay_ticks: u64,
    },
}

impl FaultAction {
    /// Apply the fault to the given World at the current time.
    ///
    /// Returns `true` if the fault was successfully applied.
    pub fn apply(&self, world: &mut World, now: Tick) -> bool {
        match self {
            FaultAction::ForceTemperature { target, value_c } => {
                if let Some(ref mut plant) = world.plant {
                    plant.apply_fault(target, "force_temperature", Some(*value_c))
                } else {
                    false
                }
            }
            FaultAction::StopHeartbeat { machine_id } => {
                // Mark the machine as stopped — it won't produce
                // events until resumed.  This is a soft pause;
                // the machine's state is preserved.
                world.stopped_machines.insert(*machine_id);
                // Record trace event.
                if let Some(machine) = world.machines.get_mut(machine_id) {
                    machine.record_trace(TraceEvent::UserU32 {
                        at: now,
                        label: "fault:stop_heartbeat",
                        value: *machine_id as u32,
                    });
                }
                true
            }
            FaultAction::Reboot {
                machine_id,
                downtime_ms,
            } => {
                // ── Stage A3 restart algorithm ──
                // 1. Emit machine_reset_begin at fault time.
                // 2. Snapshot persistent devices (flash, EEPROM, block).
                // 3. Snapshot immutable machine spec.
                // 4. Remove old machine (clears event queue, SimGlobal,
                //    volatile device state, CAN inbox).
                // 5. Store spec for reconstruction at boot_at.
                // 6. Schedule boot and mark stopped.
                // Steps 1-4 happen NOW; steps 5-7 are deferred to boot_at.
                let persistent = world
                    .machines
                    .get(machine_id)
                    .map(|m| m.snapshot_persistent_devices());

                let spec = world.machines.get(machine_id).map(|m| {
                    RestartSpec {
                        name: m.name.clone(),
                        rtos: m.rtos,
                        firmware_factory: m.firmware_factory().cloned(),
                        board: m.board_config().clone(),
                        config: m.sim_config(),
                    }
                });

                // 1. Emit machine_reset_begin BEFORE removing the old machine.
                if let Some(machine) = world.machines.get_mut(machine_id) {
                    machine.record_trace(TraceEvent::UserU32 {
                        at: now,
                        label: "machine_reset_begin",
                        value: *machine_id as u32,
                    });
                }

                // 4. Remove old machine.
                world.machines.remove(machine_id);

                if let (Some(persistent), Some(spec)) = (persistent, spec) {
                    // Store spec + persistent for deferred reconstruction.
                    world
                        .restart_specs
                        .insert(*machine_id, (spec, persistent));
                }

                // 5-6. Mark stopped and schedule boot.
                world.stopped_machines.insert(*machine_id);
                let boot_at = now + *downtime_ms * 1000;
                world.pending_boots.push((boot_at, *machine_id));
                world.pending_boots.sort_by_key(|(at, _)| *at);

                true
            }
            FaultAction::DropFrame { bus_name, frame_id } => {
                if let Some(bus) = world.buses.iter_mut().find(|b| b.name == *bus_name) {
                    bus.drop_frame(*frame_id);
                    // Record CanDrop trace event.
                    if let Some(machine) = world.machines.values_mut().next() {
                        machine.record_trace(TraceEvent::CanDrop {
                            at: now,
                            id: *frame_id,
                        });
                    }
                    true
                } else {
                    false
                }
            }
            FaultAction::DelayFrame {
                bus_name,
                frame_id,
                delay_ticks,
            } => {
                if let Some(bus) = world.buses.iter_mut().find(|b| b.name == *bus_name) {
                    bus.delay_frame(*frame_id, *delay_ticks);
                    // Record CanDelay trace event.
                    if let Some(machine) = world.machines.values_mut().next() {
                        machine.record_trace(TraceEvent::CanDelay {
                            at: now,
                            id: *frame_id,
                            extra_ticks: *delay_ticks,
                        });
                    }
                    true
                } else {
                    false
                }
            }
        }
    }
}

/// A scheduled fault: (trigger_time, action).
type ScheduledFault = (Tick, FaultAction);

/// A BLE HCI event injection action for scenario-driven Bluetooth testing.
#[derive(Debug, Clone)]
pub struct BleInjection {
    /// HCI controller ID to inject into.
    pub controller: u32,
    /// HCI packet type (1=Command, 2=AclData, 4=Event).
    pub packet_type: u8,
    /// Raw HCI event payload (after the 1-byte packet type).
    pub payload: Vec<u8>,
    /// Human-readable label for trace output.
    pub label: String,
}

/// A scheduled BLE injection: (trigger_time, injection).
type ScheduledBleInjection = (Tick, BleInjection);

use serde::{Deserialize, Serialize};

/// A serializable snapshot of World state for save/restore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldKeyframe {
    /// The current virtual time in ticks.
    pub now: Tick,
    // Simplified: just store the scenario for rebuild-on-restore.
    // A full implementation would serialize machine queues, link state, etc.
    /// TOML scenario string.
    pub scenario_toml: String,
    /// Offset maps for traces.
    pub trace_offsets: BTreeMap<u64, usize>,
}

/// Outcome of a single [`World::step`]: either events were processed at a
/// virtual time, or the run is complete (nothing left to do).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// Processed all events at this virtual time; more may remain.
    Advanced(Tick),
    /// Nothing left to do — the run is complete.
    Done,
}

/// Errors returned by fallible [`World`] operations that target a specific
/// machine by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldError {
    /// No machine with the given id exists in this World.
    MachineNotFound(u64),
}

impl std::fmt::Display for WorldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorldError::MachineNotFound(id) => write!(f, "machine {id} not found"),
        }
    }
}

impl std::error::Error for WorldError {}
/// Global event loop for multi-machine simulation.
///
/// The World is the top-level scheduling entity.  It owns:
/// - a set of [`Machine`]s, each with its own event queue and fiber runtime
/// - a set of [`Link`]s, deterministic FIFO channels between machines
/// - a set of [`CanBus`]es, broadcast buses for multi-ECU communication
/// - an optional [`EnvironmentModel`] (plant/physics model)
/// - the shared virtual clock (`now`)
///
/// On each iteration, the World:
/// 1. finds the earliest deadline across all machines, links, and buses
/// 2. advances virtual time to that deadline
/// 3. delivers any link packets and bus frames whose arrival time ≤ now
/// 4. advances all machines to now
/// 5. steps the plant model if its tick interval has elapsed
/// 6. stops when all machines are idle, all links and buses are empty,
/// Whether the World is running, paused, or stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldRunState {
    Running,
    Paused,
    Stopped,
}

///    and no plant is stepping
pub struct World {
    /// Current shared virtual time.
    pub now: Tick,

    /// Map of machine ID → Machine.
    machines: BTreeMap<u64, Machine>,

    /// Links between machines.
    links: Vec<Link>,

    /// Broadcast CAN buses.
    buses: Vec<CanBus>,

    /// Optional environment/plant model.
    plant: Option<Box<dyn EnvironmentModel>>,

    /// Plant tick interval in virtual-time ticks.
    plant_tick_interval: Tick,

    /// Virtual time of the next scheduled plant step.
    next_plant_tick: Tick,

    /// Set of machine IDs that are currently stopped (via stop_heartbeat fault).
    stopped_machines: std::collections::BTreeSet<u64>,

    /// Scheduled fault injections: (trigger_time, action).
    scheduled_faults: Vec<ScheduledFault>,

    /// Cursor into scheduled_faults for efficient processing.
    fault_cursor: usize,

    /// Scheduled BLE event injections: (trigger_time, injection).
    scheduled_ble_injections: Vec<ScheduledBleInjection>,

    /// Cursor into scheduled_ble_injections for efficient processing.
    ble_cursor: usize,

    /// Per-machine trace cursor for streaming. Initialized to 0, advanced each drain.
    trace_offsets: BTreeMap<u64, usize>,

    /// Set to false to stop the simulation.
    run_state: WorldRunState,

    /// Whether [`enable_owned_device_banks`](Self::enable_owned_device_banks)
    /// has been called. When true, a restart re-enables the reconstructed
    /// machine's owned bank so its devices stay isolated across the reboot.
    owned_banks_enabled: bool,

    /// Recorded semantic events (I2). Microcar emits typed automotive events
    /// (`vehicle_state`, `dtc_created`, …); the generic
    /// [`ContinuePredicate::Semantic`] matches over them.
    semantic_events: Vec<SemanticEvent>,


    /// Machines waiting to boot after a restart downtime elapses.
    /// Each entry is (boot_at_tick, machine_id).
    pending_boots: Vec<(Tick, u64)>,
    /// Stored immutable spec + persistent device snapshots for machines that
    /// have been removed pending a Stage A3 restart.  Keyed by machine_id;
    /// consumed by `process_pending_boots` when the downtime window elapses.
    restart_specs: BTreeMap<u64, (RestartSpec, sim_devices::PersistentDeviceState)>,
    /// Recorded named assertion failures, matched by
    /// [`ContinuePredicate::AssertionFailure`].
    assertion_failures: Vec<String>,
}

impl World {
    /// Create an empty World.
    pub fn new() -> Self {
        Self {
            now: 0,
            machines: BTreeMap::new(),
            links: Vec::new(),
            buses: Vec::new(),
            plant: None,
            plant_tick_interval: 0,
            next_plant_tick: 0,
            stopped_machines: std::collections::BTreeSet::new(),
            scheduled_faults: Vec::new(),
            fault_cursor: 0,
            scheduled_ble_injections: Vec::new(),
            ble_cursor: 0,
            pending_boots: Vec::new(),
            trace_offsets: BTreeMap::new(),
            run_state: WorldRunState::Running,
            owned_banks_enabled: false,
            restart_specs: BTreeMap::new(),
            semantic_events: Vec::new(),
            assertion_failures: Vec::new(),
        }
    }

    /// Add a machine to the World.
    ///
    /// Returns the machine ID (same as the one passed in) for chaining.
    pub fn add_machine(&mut self, machine: Machine) -> u64 {
        let id = machine.id;
        self.machines.insert(id, machine);
        id
    }

    /// Add a link between two machines.
    pub fn add_link(&mut self, link: Link) {
        self.links.push(link);
    }

    /// Add a broadcast CAN bus.
    pub fn add_bus(&mut self, bus: CanBus) {
        self.buses.push(bus);
    }

    /// Attach an environment model (plant / physics model).
    ///
    /// `tick_interval_ms` is the period between [`step`](EnvironmentModel::step)
    /// calls, in milliseconds.  Internally converted to virtual-time ticks
    /// using the same µs convention as bus timings (`ms × 1000`).
    pub fn set_plant(&mut self, plant: Box<dyn EnvironmentModel>, tick_interval_ms: u64) {
        self.plant_tick_interval = tick_interval_ms * 1000;
        self.next_plant_tick = self.plant_tick_interval;
        self.plant = Some(plant);
    }

    /// Enable per-machine device ownership (UNBLOCKING.md B1).
    ///
    /// Gives every machine in this World its own [`DeviceBank`](sim_devices::DeviceBank).
    /// After this call:
    /// - Owned banks are enabled per machine, so firmware CAN TX/RX for each
    ///   machine resolves to its private bank (two machines can use controller
    ///   ID 0 without collision).
    /// - `World::step_firmware` is the SOLE firmware-step and CAN-drain boundary
    ///   per tick. For each machine it activates that machine's execution
    ///   context (`SimGlobal` + owned bank), then stages RX into controller 0,
    ///   runs the firmware step, drains TX onto the World buses, and preserves
    ///   any leftover (unconsumed) RX back into the machine's inbox.
    /// - `Machine::advance_to` does NOT perform the extra firmware step on the
    ///   owned-bank path (B2). That extra step is retained only on the legacy
    ///   no-owned-bank path, where it is required for byte-identical golden
    ///   traces. Having a single drain boundary prevents CAN TX generated during
    ///   a late firmware step from being stranded undrained in the private
    ///   controller until a next tick that may never arrive.
    ///
    /// Without this call, all machines share the thread-local default bank
    /// (byte-identical to the pre-B1 behavior).  Call it before loading
    /// firmware so the per-machine bank is visible during
    /// [`Firmware::init`](crate::firmware::Firmware::init).
    pub fn enable_owned_device_banks(&mut self) {
        self.owned_banks_enabled = true;
        for machine in self.machines.values_mut() {
            machine.enable_owned_bank();
        }
    }
    /// Queue a driver input for the plant to apply at a specific virtual time.
    ///
    /// Delegates to the plant model's
    /// [`queue_driver_input`](EnvironmentModel::queue_driver_input).
    pub fn queue_plant_input(&mut self, at: Tick, throttle_percent: u8, brake_pressed: bool) {
        if let Some(ref mut plant) = self.plant {
            plant.queue_driver_input(at, throttle_percent, brake_pressed);
        }
    }

    /// Schedule a fault to be applied at the given virtual time.
    ///
    /// Faults are applied during the run loop when virtual time reaches
    /// `at`.  See [`FaultAction`] for the available fault types.
    pub fn schedule_fault(&mut self, at: Tick, action: FaultAction) {
        self.scheduled_faults.push((at, action));
        // Keep sorted by time.
        self.scheduled_faults.sort_by_key(|(t, _)| *t);
    }

    /// Apply all scheduled faults whose trigger time is ≤ `now`.
    ///
    /// Returns the number of faults applied.
    fn apply_scheduled_faults(&mut self, now: Tick) -> usize {
        let mut count = 0;
        while self.fault_cursor < self.scheduled_faults.len() {
            if self.scheduled_faults[self.fault_cursor].0 > now {
                break;
            }
            // remove() returns the owned tuple; elements shift left so
            // the next unprocessed item slides into cursor position.
            let (_, action) = self.scheduled_faults.remove(self.fault_cursor);
            let applied = action.apply(self, now);
            if applied {
                count += 1;
            }
        }
        count
    }

    /// Return the earliest fault trigger time, if any remain.
    fn next_fault_time(&self) -> Option<Tick> {
        if self.fault_cursor < self.scheduled_faults.len() {
            Some(self.scheduled_faults[self.fault_cursor].0)
        } else {
            None
        }
    }

    /// Schedule a BLE HCI event injection at the given virtual time.
    ///
    /// The injection will be applied during the run loop when virtual
    /// time reaches `at`.  The event is delivered to the specified
    /// HCI controller.
    pub fn schedule_ble_injection(&mut self, at: Tick, injection: BleInjection) {
        self.scheduled_ble_injections.push((at, injection));
        self.scheduled_ble_injections.sort_by_key(|(t, _)| *t);
    }

    /// Apply all scheduled BLE injections whose trigger time is ≤ `now`.
    ///
    /// Each injection pushes an HCI event into the corresponding
    /// VirtualHciController.  Returns the number of injections applied.
    fn apply_scheduled_ble_injections(&mut self, now: Tick) -> usize {
        let mut count = 0;
        while self.ble_cursor < self.scheduled_ble_injections.len() {
            if self.scheduled_ble_injections[self.ble_cursor].0 > now {
                break;
            }
            let (_, injection) = self.scheduled_ble_injections.remove(self.ble_cursor);
            // Inject into the controller.
            sim_devices::with_bt_mut(injection.controller, |bt| {
                bt.inject_event(injection.packet_type, &injection.payload);
                // Record trace event.
                if let Some(machine) = self.machines.values_mut().next() {
                    machine.record_trace(sim_core::TraceEvent::UserU32 {
                        at: now,
                        label: Box::leak(injection.label.into_boxed_str()),
                        value: injection.controller,
                    });
                }
            });
            count += 1;
        }
        count
    }

    /// Return the earliest BLE injection time, if any remain.
    fn next_ble_time(&self) -> Option<Tick> {
        if self.ble_cursor < self.scheduled_ble_injections.len() {
            Some(self.scheduled_ble_injections[self.ble_cursor].0)
        } else {
            None
        }
    }

    /// Get a reference to a machine by ID.
    pub fn machine(&self, id: u64) -> Option<&Machine> {
        self.machines.get(&id)
    }

    /// Get a mutable reference to a machine by ID.
    pub fn machine_mut(&mut self, id: u64) -> Option<&mut Machine> {
        self.machines.get_mut(&id)
    }

    /// Return the number of machines in the World.
    pub fn machine_count(&self) -> usize {
        self.machines.len()
    }

    /// Run `f` with exactly the target machine's device context active.
    ///
    /// Resolves exactly one machine by id and executes `f` in that machine's
    /// context via [`Machine::with_device_context`]. It never falls back to the
    /// most recently active machine: a missing target returns
    /// [`WorldError::MachineNotFound`].
    pub fn with_machine_devices<R>(
        &self,
        machine_id: u64,
        f: impl FnOnce() -> R,
    ) -> Result<R, WorldError> {
        let machine = self
            .machines
            .get(&machine_id)
            .ok_or(WorldError::MachineNotFound(machine_id))?;
        Ok(machine.with_device_context(f))
    }

    /// Configure the board of a specific machine inside its device context.
    ///
    /// Returns the number of peripherals initialised, or [`WorldError::MachineNotFound`].
    pub fn configure_machine_board(
        &mut self,
        machine_id: u64,
        board: BoardConfig,
    ) -> Result<usize, WorldError> {
        let machine = self
            .machines
            .get_mut(&machine_id)
            .ok_or(WorldError::MachineNotFound(machine_id))?;
        machine
            .configure_board(board)
            .map_err(|_e| WorldError::MachineNotFound(machine_id))
    }

    /// Iterate the machine ids present in this World, in ascending order.
    pub fn machine_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.machines.keys().copied()
    }

    /// Return the number of links in the World.
    pub fn link_count(&self) -> usize {
        self.links.len()
    }

    /// Return the number of CAN buses in the World.
    pub fn bus_count(&self) -> usize {
        self.buses.len()
    }

    /// Return a mutable reference to a bus by name.
    pub fn bus_mut(&mut self, name: &str) -> Option<&mut CanBus> {
        self.buses.iter_mut().find(|b| b.name == name)
    }

    /// Return an iterator over all machines (in ID order).
    pub fn machines(&self) -> impl Iterator<Item = &Machine> {
        self.machines.values()
    }

    /// Return a slice of all links.
    pub fn links(&self) -> &[Link] {
        &self.links
    }

    /// Return a slice of all buses.
    pub fn buses(&self) -> &[CanBus] {
        &self.buses
    }

    /// Check if all machines are idle and all links and buses are empty.
    pub fn all_idle(&self) -> bool {
        self.machines.values().all(|m| m.is_idle())
            && self.links.iter().all(|l| l.pending_count() == 0)
            && self.buses.iter().all(|b| b.pending_count() == 0)
            && self.ble_cursor >= self.scheduled_ble_injections.len()
    }

    /// Pre-load a packet into a link for timed injection.
    ///
    /// This is the scenario-injection path: the data is placed into the
    /// link at time `at`, and the World's event loop will deliver it
    /// after the link's configured latency.
    ///
    /// Returns `true` if the link was found and the injection succeeded.
    pub fn inject_packet(&mut self, from: u64, to: u64, data: &[u8], at: Tick) -> bool {
        for link in &mut self.links {
            if link.source() == from && link.target() == to {
                link.send(data, at);
                return true;
            }
        }
        false
    }

    /// Inject a CAN frame onto a named bus for timed delivery.
    ///
    /// Returns the number of receivers the frame was queued for,
    /// or 0 if the bus was not found.
    pub fn inject_can_frame(
        &mut self,
        bus_name: &str,
        sender: u64,
        frame_id: u32,
        data: &[u8],
        at: Tick,
    ) -> usize {
        if let Some(bus) = self.buses.iter_mut().find(|b| b.name == bus_name) {
            bus.send(sender, frame_id, data, at)
        } else {
            0
        }
    }

    /// Find the earliest deadline across all machines, links, and buses
    /// plus the next plant tick if a plant is attached, plus scheduled faults.
    ///
    /// Returns `None` if everything is idle and no plant is stepping.
    pub fn next_global_event_time(&self) -> Option<Tick> {
        let mut earliest: Option<Tick> = None;

        // Check all machines' next event times.
        for machine in self.machines.values() {
            // Skip stopped machines.
            if self.stopped_machines.contains(&machine.id) {
                continue;
            }
            if let Some(t) = machine.next_event_time() {
                earliest = Some(earliest.map_or(t, |e| e.min(t)));
            }
        }

        // Check all links' next arrival times.
        for link in &self.links {
            if let Some(t) = link.next_arrival_time() {
                earliest = Some(earliest.map_or(t, |e| e.min(t)));
            }
        }

        // Check all buses' next arrival times.
        for bus in &self.buses {
            if let Some(t) = bus.next_arrival_time() {
                earliest = Some(earliest.map_or(t, |e| e.min(t)));
            }
        }

        // Include next plant tick if a plant is attached.
        if self.plant.is_some() && self.plant_tick_interval > 0 {
            earliest = Some(earliest.map_or(self.next_plant_tick, |e| e.min(self.next_plant_tick)));
        }

        // Include next fault time.
        if let Some(ft) = self.next_fault_time() {
            earliest = Some(earliest.map_or(ft, |e| e.min(ft)));
        }

        // Include next BLE injection time.
        if let Some(bt) = self.next_ble_time() {
            earliest = Some(earliest.map_or(bt, |e| e.min(bt)));
        }

        // Include the earliest pending restart boot time so the World
        // advances to the boot moment even when no other event exists.
        if let Some(&(boot_at, _)) = self.pending_boots.first() {
            earliest = Some(earliest.map_or(boot_at, |e| e.min(boot_at)));
        }

        earliest
    }

    /// Deliver all link packets whose arrival time ≤ `now`.
    ///
    /// For each delivered packet, records a `PacketRx` trace event
    /// on the target machine.  For Ethernet links, also injects the
    /// frame into ETH_DEVICES[0]'s RX queue so firmware can receive it.
    fn deliver_links(&mut self, now: Tick) {
        // Collect deliveries per target machine.
        let mut deliveries: BTreeMap<u64, Vec<(Tick, usize)>> = BTreeMap::new();

        for link in &mut self.links {
            let target_id = link.target();
            let is_eth = link.is_eth();
            let arrived = link.drain_arrived(now);
            if arrived.is_empty() {
                continue;
            }
            for pkt in &arrived {
                deliveries
                    .entry(target_id)
                    .or_default()
                    .push((now, pkt.len()));
                // ── Inject Eth frames into firmware ETH_DEVICES[0] RX ──
                if is_eth {
                    sim_net::with_eth_device_mut(0, |eth| eth.inject_rx(pkt.clone()));
                }
            }
        }

        // Record trace events on target machines.
        for (target_id, pkts) in &deliveries {
            if let Some(target) = self.machines.get_mut(target_id) {
                for &(at, len) in pkts {
                    target.record_trace(TraceEvent::PacketRx { at, len });
                }
            }
        }
    }

    /// Deliver all CAN bus frames whose arrival time ≤ `now`.
    ///
    /// For each delivered frame, injects it into CAN controller 0's RX
    /// queue (so firmware can receive it), and records a `CanRx` trace
    /// event on the receiver machine and a `CanTx` trace event on the
    /// sender machine.
    fn deliver_buses(&mut self, now: Tick) {
        for bus in &mut self.buses {
            let frames = bus.drain_arrived(now);
            for (receiver_id, sender_id, frame_id, data) in &frames {
                // ── Inject into the receiver's private CAN RX queue ──
                // When owned banks are enabled, each machine has its own CAN
                // controller 0.  We activate the receiver's device context so
                // frames land in *its* controller 0, never another machine's.
                // For legacy (no owned bank) single-simulator paths, the
                // fallback default bank is used.
                {
                    let can_frame = CanFrame::new_data(*frame_id, data);
                    if let Some(receiver) = self.machines.get(receiver_id) {
                        receiver.with_device_context(|| {
                            sim_devices::with_can_mut(0, |can| can.inject_rx(can_frame));
                        });
                    }
                }

                // Record CanRx on the receiver.
                if let Some(receiver) = self.machines.get_mut(receiver_id) {
                    receiver.record_trace(TraceEvent::CanRx {
                        at: now,
                        receiver: *receiver_id,
                        id: *frame_id,
                        len: data.len(),
                    });
                }
                // Record CanTx on the sender.
                if let Some(sender) = self.machines.get_mut(sender_id) {
                    sender.record_trace(TraceEvent::CanTx {
                        at: now,
                        sender: *sender_id,
                        id: *frame_id,
                        len: data.len(),
                    });
                }
            }
        }
    }

    /// Advance all machines to the given virtual time.
    fn advance_machines_to(&mut self, deadline: Tick) -> Result<(), SimError> {
        for machine in self.machines.values_mut() {
            machine.advance_to(deadline)?;
        }
        Ok(())
    }

    /// Step firmware on all machines that have firmware loaded.
    ///
    /// Firmware is temporarily taken out of each machine to avoid
    /// borrow conflicts: the firmware receives `&mut Machine` while
    /// the firmware itself is moved out of the machine.
    ///
    /// After each machine's firmware step:
    /// - CAN frames sent via `sim_can_send` are drained from CAN controller 0's
    ///   TX queue and injected onto the World CanBus for delivery.
    /// - Ethernet frames sent via `sim_eth_send` are drained from ETH_DEVICES[0]
    ///   and injected onto World Ethernet links for delivery.
    /// - BT HCI commands are processed to generate auto-responses.
    fn step_firmware(&mut self, now: Tick) {
        // Collect firmware from all machines.
        let mut firmwares: Vec<(u64, Box<dyn Firmware>)> = Vec::new();
        for (id, machine) in self.machines.iter_mut() {
            if let Some(fw) = machine.take_firmware() {
                firmwares.push((*id, fw));
            }
        }

        // Step each firmware with its host machine, then drain TX from each
        // machine's *private* device context — not the default bank.
        for (id, mut fw) in firmwares.drain(..) {
            if let Some(machine) = self.machines.get_mut(&id) {
                fw.step(now, machine);

                // ── Bridge CAN TX: drain firmware CAN sends → World CanBus ──
                // Activate this machine's device context so
                // `sim_devices::with_can_mut(0, …)` resolves to *its* private
                // CAN controller 0, not another machine's or the default bank.
                machine.with_device_context(|| {
                    loop {
                        let frame = sim_devices::with_can_mut(0, |can| {
                            if can.tx_queue.is_empty() {
                                None
                            } else {
                                Some(can.tx_queue.remove(0))
                            }
                        });
                        match frame {
                            Some(Some(f)) => {
                                let payload = &f.data[..f.dlc as usize];
                                for bus in &mut self.buses {
                                    bus.send(id, f.id, payload, now);
                                }
                            }
                            _ => break,
                        }
                    }

                    // ── Bridge Ethernet TX: drain firmware eth sends → World Eth links ──
                    loop {
                        let frames = sim_net::with_eth_device_mut(0, |eth| {
                            if eth.has_tx() {
                                Some(eth.drain_tx())
                            } else {
                                None
                            }
                        });
                        match frames {
                            Some(Some(frames)) => {
                                for frame in frames {
                                    // Send onto every World Eth link where this
                                    // machine is the source.
                                    for link in &mut self.links {
                                        if link.is_eth() && link.source() == id {
                                            link.send(&frame, now);
                                        }
                                    }
                                }
                            }
                            _ => break,
                        }
                    }
                });

                machine.set_firmware(fw);
            }
        }

        // ── Process BT commands on all controllers ──
        // Auto-generate HCI event responses for any pending commands.
        let ctrl_ids: Vec<u32> = sim_devices::bt_ids();
        for cid in ctrl_ids {
            sim_devices::with_bt_mut(cid, |bt| {
                if bt.has_commands() {
                    bt.process_commands();
                }
            });
        }
    }

    /// Step the plant model if the current time has reached or passed
    /// the next plant tick.
    ///
    /// The plant is stepped once per elapsed `plant_tick_interval`,
    /// then the next tick time is scheduled.  If time jumped past
    /// multiple intervals, the plant is stepped for each one.
    fn step_plant(&mut self, now: Tick) {
        if self.plant.is_none() || self.plant_tick_interval == 0 {
            return;
        }

        while now >= self.next_plant_tick {
            // Take the plant out, step it, put it back.
            // This avoids borrow conflicts when the plant needs
            // &mut World access via its step method.
            let mut plant = self.plant.take().unwrap();
            plant.step(now, self);
            self.plant = Some(plant);

            self.next_plant_tick = self
                .next_plant_tick
                .saturating_add(self.plant_tick_interval);
        }
    }

    /// Reconstruct a machine that was removed for a Stage A3 restart, using its
    /// stored immutable spec and persistent device snapshot.  Enables owned bank
    /// if the World has them, configures the board, restores persistent devices,
    /// attaches firmware from the factory (running the normal boot path), and
    /// emits `machine_reset_boot`.
    fn boot_machine_from_spec(
        &mut self,
        machine_id: u64,
        now: Tick,
        spec: RestartSpec,
        persistent: sim_devices::PersistentDeviceState,
    ) {
        self.stopped_machines.remove(&machine_id);
        let mut new_machine = Machine::new(machine_id, &spec.name, spec.config);
        new_machine.rtos = spec.rtos;
        if self.owned_banks_enabled {
            new_machine.enable_owned_bank();
        }
        if let Some(factory) = spec.firmware_factory.as_ref() {
            new_machine.set_firmware_factory(factory.clone());
        }
        let _ = new_machine.configure_board(spec.board);
        new_machine.restore_persistent_devices(persistent);
        if let Some(factory) = spec.firmware_factory {
            let firmware = factory();
            new_machine.load_firmware(firmware);
        }
        new_machine.record_trace(TraceEvent::UserU32 {
            at: now,
            label: "machine_reset_boot",
            value: machine_id as u32,
        });
        self.machines.insert(machine_id, new_machine);
    }

    /// Boot a machine that was stopped (via stop_heartbeat) but never removed.
    /// Attaches firmware from its factory if present.
    fn boot_machine(&mut self, machine_id: u64, now: Tick) {
        self.stopped_machines.remove(&machine_id);
        let factory = self
            .machines
            .get(&machine_id)
            .and_then(|m| m.firmware_factory());
        if let Some(factory) = factory {
            let firmware = factory();
            if let Some(machine) = self.machines.get_mut(&machine_id) {
                machine.load_firmware(firmware);
            }
        }
        if let Some(machine) = self.machines.get_mut(&machine_id) {
            machine.record_trace(TraceEvent::UserU32 {
                at: now,
                label: "machine_reset_boot",
                value: machine_id as u32,
            });
        }
    }

    /// Boot any machines whose scheduled restart downtime has elapsed
    /// (`boot_time <= now`).  For Stage A3 restarts, reconstructs the machine
    /// from its stored immutable spec + persistent devices before booting.
    fn process_pending_boots(&mut self, now: Tick) {
        if self.pending_boots.is_empty() {
            return;
        }
        let due: Vec<u64> = self
            .pending_boots
            .iter()
            .filter(|(at, _)| *at <= now)
            .map(|(_, id)| *id)
            .collect();
        self.pending_boots.retain(|(at, _)| *at > now);
        for id in due {
            if let Some((spec, persistent)) = self.restart_specs.remove(&id) {
                self.boot_machine_from_spec(id, now, spec, persistent);
            } else if self.machines.contains_key(&id) {
                self.boot_machine(id, now);
            }
        }
    }

    /// Advance the simulation by one virtual-time step: process every event at
    /// the next global event time (link/bus delivery, faults, BLE injections,
    /// firmware, machine advance, plant), then report whether more work remains.
    ///
    /// [`run`](Self::run) is exactly `while running { step()? }` until
    /// [`StepOutcome::Done`], so a stepped replay is trace-identical to a
    /// continuous run — the basis for the debug_gym "stepped == continuous"
    /// invariant — and [`continue_until`](Self::continue_until) is built on it.
    pub fn step(&mut self) -> Result<StepOutcome, SimError> {
        let Some(t) = self.next_global_event_time() else {
            return Ok(StepOutcome::Done);
        };
        // Time must not go backwards.
        if t < self.now {
            return Err(SimError::TimeWentBackwards {
                now: self.now,
                event_at: t,
            });
        }

        self.now = t;
        // 1. Deliver link packets at this time.
        self.deliver_links(self.now);
        // 2. Boot any machines whose restart downtime has elapsed BEFORE bus
        //    delivery, so a frame arriving exactly at boot_at reaches the
        //    freshly-booted (running) machine, while arrivals during downtime
        //    were dropped to the stopped receiver (A3 delivery boundary).
        self.process_pending_boots(self.now);
        // 3. Deliver bus frames at this time.
        self.deliver_buses(self.now);
        // 4. Apply scheduled faults.
        self.apply_scheduled_faults(self.now);
        // 5. Apply scheduled BLE injections.
        self.apply_scheduled_ble_injections(self.now);
        // 6. Step firmware on all machines.
        self.step_firmware(self.now);
        // 4. Advance all machines to this time.
        self.advance_machines_to(self.now)?;
        // 5. Step the plant model (may be a no-op if no plant or not yet due).
        self.step_plant(self.now);

        // Stop condition: all machines idle, links/buses empty, no plant
        // (a plant keeps the simulation alive), and no machine waiting to boot
        // after a restart downtime.
        if self.all_idle() && self.plant.is_none() && self.pending_boots.is_empty() {
            Ok(StepOutcome::Done)
        } else {
            Ok(StepOutcome::Advanced(t))
        }
    }
    /// Run the simulation until all machines are idle, all links
    /// and buses are empty, no plant is stepping, and no pending
    /// restart boots remain — or until [`stop`](Self::stop) is called.
    ///
    /// Delegates to [`step`](Self::step) so every product path (gRPC,
    /// JSON-RPC, CLI binary) shares the same core loop and no path
    /// accidentally skips `process_pending_boots`.
    pub fn run(&mut self) -> Result<(), SimError> {
        while self.is_running() {
            match self.step()? {
                StepOutcome::Advanced(_) => {}
                StepOutcome::Done => break,
            }
        }
        Ok(())
    }

    /// Run the simulation until the given deadline, all machines are idle,
    /// and no pending restart boots remain — or until [`stop`](Self::stop) is
    /// called.
    ///
    /// After the loop, `self.now` will be at most `deadline`.
    /// Delegates to [`step`](Self::step) for the same reason as [`run`](Self::run).
    pub fn run_until(&mut self, deadline: Tick) -> Result<(), SimError> {
        while self.is_running() && self.now < deadline {
            match self.next_global_event_time() {
                Some(t) if t <= deadline => {
                    match self.step()? {
                        StepOutcome::Advanced(_) => {}
                        StepOutcome::Done => break,
                    }
                }
                _ => break,
            }
        }
        Ok(())
    }

    /// Record a typed semantic event (I2). Microcar names the automotive fields
    /// (`mode`, `code`, …); costar stores them generically.
    pub fn record_semantic_event(
        &mut self,
        machine_id: u64,
        event_type: impl Into<String>,
        fields: std::collections::BTreeMap<String, ScalarValue>,
    ) {
        self.semantic_events.push(SemanticEvent {
            machine_id: Some(machine_id),
            event_type: event_type.into(),
            fields,
        });
    }

    /// All recorded semantic events.
    pub fn semantic_events(&self) -> &[SemanticEvent] {
        &self.semantic_events
    }

    /// Record a named assertion failure (I2).
    pub fn record_assertion_failure(&mut self, name: impl Into<String>) {
        self.assertion_failures.push(name.into());
    }

    /// All recorded assertion-failure names.
    pub fn assertion_failures(&self) -> &[String] {
        &self.assertion_failures
    }

    /// Run the simulation step-by-step until `pred` returns `true`, the
    /// `deadline` is reached, or the run completes naturally.  Returns
    /// `Ok(true)` when the predicate was satisfied, `Ok(false)` when the
    /// deadline expired or the world finished first.
    pub fn continue_until(
        &mut self,
        pred: impl Fn(&World) -> bool,
        deadline: Tick,
    ) -> Result<bool, SimError> {
        while self.is_running() && self.now < deadline {
            if pred(self) {
                return Ok(true);
            }
            self.step()?;
        }
        Ok(false)
    }

    /// Run until a typed [`ContinuePredicate`] holds, the deadline is reached,
    /// or the run completes. Reuses [`continue_until`](Self::continue_until) —
    /// there is no second scheduler loop.
    pub fn continue_until_predicate(
        &mut self,
        predicate: &ContinuePredicate,
        deadline: Tick,
    ) -> Result<bool, SimError> {
        self.continue_until(|w| predicate.holds(w), deadline)
    }

    /// Message breakpoint: run until a CAN frame with `frame_id` is delivered
    /// (a `can-rx` for that id appears in any machine's trace), the `deadline`
    /// is reached, or the run completes. Returns whether the breakpoint was hit.
    /// Built on [`continue_until`](Self::continue_until) — the plan's "breakpoint
    /// predicate for message".
    pub fn run_to_frame(&mut self, frame_id: u32, deadline: Tick) -> Result<bool, SimError> {
        let needle = format!("id={frame_id:#06x}");
        self.continue_until(
            |w| {
                w.drain_all_traces()
                    .iter()
                    .any(|l| l.contains("can-rx") && l.contains(&needle))
            },
            deadline,
        )
    }

    /// Stop the simulation at the next iteration boundary.
    pub fn stop(&mut self) {
        self.run_state = WorldRunState::Stopped;
    }

    /// Collect all trace events from all machines, interleaved in
    /// timestamp order with machine ID prefixes.
    pub fn drain_all_traces(&self) -> Vec<String> {
        let mut all: Vec<String> = Vec::new();
        for machine in self.machines.values() {
            all.extend(machine.drain_trace_prefixed());
        }
        all
    }

    /// Pause the simulation. The run loop will stop advancing after the
    /// current iteration.  Use [`resume`](Self::resume) to continue.
    pub fn pause(&mut self) {
        self.run_state = WorldRunState::Paused;
    }

    /// Resume the simulation after a pause.
    pub fn resume(&mut self) {
        self.run_state = WorldRunState::Running;
    }

    /// Return true if the simulation is paused.
    pub fn is_paused(&self) -> bool {
        matches!(self.run_state, WorldRunState::Paused)
    }

    /// Return true if the simulation has been stopped.
    pub fn is_stopped(&self) -> bool {
        matches!(self.run_state, WorldRunState::Stopped)
    }

    /// Return true if the simulation is running (neither paused nor stopped).
    pub fn is_running(&self) -> bool {
        matches!(self.run_state, WorldRunState::Running)
    }

    /// Return true if the World has an environment/plant model attached.
    pub fn has_plant(&self) -> bool {
        self.plant.is_some()
    }

    /// Drain new trace events since the last call, per machine.
    /// Returns machine-prefixed trace lines like "[machine.0]    100 task-resume id=1 ..."
    pub fn drain_new_traces(&mut self) -> Vec<String> {
        let mut all = Vec::new();
        for machine in self.machines.values() {
            let offset = self.trace_offsets.entry(machine.id).or_insert(0);
            let events = machine.trace().events();
            let prefix = format!("[machine.{}]", machine.id);
            for ev in &events[*offset..] {
                all.push(format!("{} {}", prefix, ev));
            }
            *offset = events.len();
        }
        all
    }

    /// Save a keyframe of the current simulation state.
    pub fn save_keyframe(&mut self, scenario_toml: String) -> WorldKeyframe {
        // Collect current trace offsets.
        for machine in self.machines.values() {
            self.trace_offsets
                .entry(machine.id)
                .and_modify(|o| *o = machine.trace().events().len())
                .or_insert(machine.trace().events().len());
        }
        WorldKeyframe {
            now: self.now,
            scenario_toml,
            trace_offsets: self.trace_offsets.clone(),
        }
    }

    /// Serialize a keyframe to a JSON byte vector for storage.
    pub fn serialize_keyframe(kf: &WorldKeyframe) -> Result<Vec<u8>, String> {
        serde_json::to_vec(kf).map_err(|e| format!("keyframe serialize error: {}", e))
    }

    /// Deserialize a keyframe from a JSON byte vector.
    pub fn deserialize_keyframe(data: &[u8]) -> Result<WorldKeyframe, String> {
        serde_json::from_slice(data).map_err(|e| format!("keyframe deserialize error: {}", e))
    }

    /// Load a keyframe, restoring the simulation state.
    /// In this simplified version, the caller rewinds the World to the
    /// keyframe by rebuilding from the stored scenario and running to now.
    pub fn load_keyframe(&mut self, kf: &WorldKeyframe) {
        self.now = kf.now;
        self.trace_offsets = kf.trace_offsets.clone();
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

// Safety: World is used single-threaded or behind a Mutex.
// EventCallback closures are always Send in practice.
unsafe impl Send for World {}
unsafe impl Sync for World {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_world_creation() {
        let world = World::new();
        assert_eq!(world.machine_count(), 0);
        assert_eq!(world.link_count(), 0);
        assert_eq!(world.now, 0);
        assert_eq!(world.next_global_event_time(), None);
    }

    #[test]
    fn test_world_with_one_idle_machine() {
        let mut world = World::new();
        let m = Machine::with_defaults(0, "m0");
        world.add_machine(m);

        assert_eq!(world.machine_count(), 1);
        assert_eq!(world.next_global_event_time(), None);
    }

    #[test]
    fn test_world_with_one_active_machine() {
        let mut world = World::new();
        let mut m = Machine::with_defaults(0, "m0");
        m.schedule_at(10, 0, "test", Box::new(|_| {}));
        world.add_machine(m);

        assert_eq!(world.next_global_event_time(), Some(10));

        world.run().unwrap();
        assert_eq!(world.now, 10);
        assert!(world.all_idle());
    }

    #[test]
    fn test_world_with_two_machines_lockstep() {
        let mut world = World::new();

        let mut m0 = Machine::with_defaults(0, "m0");
        m0.schedule_at(5, 0, "early", Box::new(|_| {}));
        m0.schedule_at(15, 0, "late", Box::new(|_| {}));
        world.add_machine(m0);

        let mut m1 = Machine::with_defaults(1, "m1");
        m1.schedule_at(10, 0, "mid", Box::new(|_| {}));
        world.add_machine(m1);

        // Earliest event is at time 5 (m0).
        assert_eq!(world.next_global_event_time(), Some(5));

        world.run().unwrap();

        // After run, all events dispatched.
        assert_eq!(world.now, 15);
        assert!(world.all_idle());
    }

    #[test]
    fn test_world_with_link_delivery() {
        let mut world = World::new();

        let m0 = Machine::with_defaults(0, "sender");
        let m1 = Machine::with_defaults(1, "receiver");
        world.add_machine(m0);
        world.add_machine(m1);

        // Link from m0→m1 with 5-tick latency.
        let mut link = Link::new_fifo(0, 1, 5);
        link.send(b"hello", 0);
        world.add_link(link);

        // Link arrival at time 5.
        assert_eq!(world.next_global_event_time(), Some(5));

        world.run().unwrap();
        assert_eq!(world.now, 5);
        assert!(world.all_idle());

        // Verify PacketRx was recorded on machine 1.
        let m1 = world.machine(1).unwrap();
        let traces = m1.drain_trace_prefixed();
        assert_eq!(traces.len(), 1);
        assert!(traces[0].contains("pkt-rx"));
        assert!(traces[0].contains("5"));
    }

    #[test]
    fn test_world_run_until_deadline() {
        let mut world = World::new();

        let mut m0 = Machine::with_defaults(0, "m0");
        m0.schedule_at(100, 0, "far-future", Box::new(|_| {}));
        world.add_machine(m0);

        // Run only until time 50 — the event at 100 should NOT fire.
        world.run_until(50).unwrap();
        assert_eq!(world.now, 0); // No events <= 50
        assert!(!world.all_idle()); // Event at 100 still pending

        // Run until 200 — event at 100 fires.
        world.run_until(200).unwrap();
        assert_eq!(world.now, 100);
        assert!(world.all_idle());
    }

    #[test]
    fn test_world_link_and_machine_events_interleaved() {
        let mut world = World::new();

        // Machine 0: event at time 7.
        let mut m0 = Machine::with_defaults(0, "m0");
        m0.schedule_at(7, 0, "m0-event", Box::new(|_| {}));
        world.add_machine(m0);

        let m1 = Machine::with_defaults(1, "m1");
        world.add_machine(m1);

        // Link m0→m1 with 5-tick latency, send at time 0.
        let mut link = Link::new_fifo(0, 1, 5);
        link.send(b"packet", 0);
        world.add_link(link);

        // Earliest: link arrival at time 5.
        assert_eq!(world.next_global_event_time(), Some(5));

        world.run().unwrap();

        // Link delivered at 5, then m0 event at 7.
        assert_eq!(world.now, 7);
        assert!(world.all_idle());
    }

    #[test]
    fn test_world_plant_tick_scheduling() {
        let mut world = World::new();

        // Use Arc<AtomicU32> for 'static lifetime since Box<dyn EnvironmentModel>
        // requires 'static bounds.
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let call_count: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));

        struct CountingPlant {
            count: Arc<AtomicU32>,
        }
        impl EnvironmentModel for CountingPlant {
            fn step(&mut self, _now: Tick, _world: &mut World) {
                self.count.fetch_add(1, Ordering::SeqCst);
            }
            fn queue_driver_input(&mut self, _at: Tick, _throttle: u8, _brake: bool) {}
        }

        world.set_plant(
            Box::new(CountingPlant {
                count: call_count.clone(),
            }),
            10,
        );
        // Override plant tick interval and next tick to 100 for fast test.
        world.plant_tick_interval = 100;
        world.next_plant_tick = 100;

        // next_global_event_time should include plant tick.
        assert_eq!(world.next_global_event_time(), Some(100));

        // Step plant manually at time 250 (should fire at 100 and 200).
        world.now = 250;
        world.next_plant_tick = 100;
        world.step_plant(250);

        assert_eq!(call_count.load(Ordering::SeqCst), 2); // stepped at 100 and 200
        assert_eq!(world.next_plant_tick, 300); // next at 300

        // Step again at 300.
        world.now = 300;
        world.step_plant(300);
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn test_world_plant_next_event_includes_plant_tick() {
        let mut world = World::new();

        struct NoopPlant;
        impl EnvironmentModel for NoopPlant {
            fn step(&mut self, _now: Tick, _world: &mut World) {}
            fn queue_driver_input(&mut self, _at: Tick, _throttle: u8, _brake: bool) {}
        }

        world.set_plant(Box::new(NoopPlant), 50);
        // plant_tick_interval = 50 * 1000 = 50000 ticks

        // No machines, but plant tick scheduled.
        assert_eq!(world.next_global_event_time(), Some(50000));

        // Plant-only world: use run_until with a deadline since run()
        // with a plant-only world runs forever (the plant keeps ticking).
        world.run_until(120000).unwrap();
        // Plant should have stepped at 50000 and 100000.
        assert_eq!(world.now, 100000);
    }

    #[test]
    fn test_world_plant_with_idle_machines() {
        let mut world = World::new();

        struct TickCountPlant {
            ticks: u32,
        }
        impl EnvironmentModel for TickCountPlant {
            fn step(&mut self, _now: Tick, _world: &mut World) {
                self.ticks += 1;
            }
            fn queue_driver_input(&mut self, _at: Tick, _throttle: u8, _brake: bool) {}
        }

        // Add an idle machine (no events).
        let m0 = Machine::with_defaults(0, "m0");
        world.add_machine(m0);

        world.set_plant(Box::new(TickCountPlant { ticks: 0 }), 1); // 1ms = 1000 ticks
        world.plant_tick_interval = 100; // speed up: 100 ticks

        // Run until 500 — plant should step at 100, 200, 300, 400, 500.
        world.run_until(500).unwrap();

        // Should have 5 plant ticks.
        let _plant = world.plant.take().unwrap();
        // Can't inspect ticks without downcast, just verify it ran.
    }

    #[test]
    fn test_world_with_firmware_stepping() {
        use crate::firmware::Firmware;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let step_count: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));

        struct CountingFirmware {
            count: Arc<AtomicU32>,
        }
        impl Firmware for CountingFirmware {
            fn step(&mut self, _now: Tick, _machine: &mut Machine) {
                self.count.fetch_add(1, Ordering::SeqCst);
            }
        }

        let mut world = World::new();

        // Add a machine with an event at time 10.
        let mut m0 = Machine::with_defaults(0, "m0");
        m0.schedule_at(10, 0, "test", Box::new(|_| {}));

        // Load firmware onto the machine.
        m0.load_firmware(Box::new(CountingFirmware {
            count: step_count.clone(),
        }));

        world.add_machine(m0);

        // Run the simulation.
        world.run().unwrap();

        // The World's run loop steps once at time 10.
        // step_firmware is called on that iteration.
        assert_eq!(world.now, 10);
        // Note: machine with firmware is never "idle" — but the World
        // stops when no events remain, not when all machines are idle.
        assert!(step_count.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn test_world_with_multiple_firmware_stepping() {
        use crate::firmware::Firmware;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let step_count_0: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
        let step_count_1: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));

        struct CountingFirmware {
            count: Arc<AtomicU32>,
        }
        impl Firmware for CountingFirmware {
            fn step(&mut self, _now: Tick, _machine: &mut Machine) {
                self.count.fetch_add(1, Ordering::SeqCst);
            }
        }

        let mut world = World::new();

        // Machine 0: event at time 5.
        let mut m0 = Machine::with_defaults(0, "m0");
        m0.schedule_at(5, 0, "early", Box::new(|_| {}));
        m0.load_firmware(Box::new(CountingFirmware {
            count: step_count_0.clone(),
        }));
        world.add_machine(m0);

        // Machine 1: event at time 15.
        let mut m1 = Machine::with_defaults(1, "m1");
        m1.schedule_at(15, 0, "late", Box::new(|_| {}));
        m1.load_firmware(Box::new(CountingFirmware {
            count: step_count_1.clone(),
        }));
        world.add_machine(m1);

        world.run().unwrap();
        assert_eq!(world.now, 15);

        // Both firmwares were stepped at least once.
        // (Machine 0's firmware was stepped at tick 5; both were stepped at tick 15.)
        assert!(step_count_0.load(Ordering::SeqCst) >= 1);
        assert!(step_count_1.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn test_world_firmware_no_firmware_no_panic() {
        // Regression: ensure step_firmware is a no-op when no machines have firmware.
        let mut world = World::new();

        let mut m0 = Machine::with_defaults(0, "m0");
        m0.schedule_at(5, 0, "test", Box::new(|_| {}));
        world.add_machine(m0);

        // No firmware loaded — run should not panic.
        world.run().unwrap();
        assert_eq!(world.now, 5);
    }

    #[test]
    fn test_pause_resume() {
        let mut world = World::new();
        assert!(world.is_running());
        assert!(!world.is_paused());
        assert!(!world.is_stopped());
        world.pause();
        assert!(world.is_paused());
        assert!(!world.is_running());
        assert!(!world.is_stopped());
        world.resume();
        assert!(!world.is_paused());
        assert!(world.is_running());
        assert!(!world.is_stopped());
        world.stop();
        assert!(world.is_stopped());
        assert!(!world.is_paused());
        assert!(!world.is_running());
    }

    #[test]
    fn test_drain_new_traces() {
        let mut world = World::new();
        let mut m = Machine::with_defaults(0, "m0");
        m.record_trace(TraceEvent::PacketRx { at: 10, len: 42 });
        world.add_machine(m);

        let traces = world.drain_new_traces();
        assert_eq!(traces.len(), 1);
        assert!(traces[0].contains("[machine.0]"));
        assert!(traces[0].contains("pkt-rx"));

        // Second drain should be empty (offset advanced).
        let traces2 = world.drain_new_traces();
        assert!(traces2.is_empty());
    }

    #[test]
    fn test_drain_new_traces_incremental() {
        let mut world = World::new();
        let mut m = Machine::with_defaults(0, "m0");
        m.record_trace(TraceEvent::PacketRx { at: 10, len: 42 });
        world.add_machine(m);

        // First drain captures the one event.
        let traces = world.drain_new_traces();
        assert_eq!(traces.len(), 1);

        // Record another event.
        if let Some(machine) = world.machine_mut(0) {
            machine.record_trace(TraceEvent::PacketRx { at: 20, len: 99 });
        }

        // Second drain captures only the new event.
        let traces2 = world.drain_new_traces();
        assert_eq!(traces2.len(), 1);
        assert!(traces2[0].contains("99"));
    }

    #[test]
    fn test_keyframe_save_load() {
        let mut world = World::new();
        world.now = 500;
        let kf = world.save_keyframe(String::new());
        assert_eq!(kf.now, 500);

        world.now = 0;
        world.load_keyframe(&kf);
        assert_eq!(world.now, 500);
    }

    #[test]
    fn test_keyframe_preserves_trace_offsets() {
        let mut world = World::new();
        let mut m = Machine::with_defaults(0, "m0");
        m.record_trace(TraceEvent::PacketRx { at: 10, len: 42 });
        world.add_machine(m);

        // Drain to advance the offset.
        let _traces = world.drain_new_traces();

        // Second drain should be empty (offset advanced to 1).
        let empty = world.drain_new_traces();
        assert!(empty.is_empty());

        // Save keyframe (captures offset=1).
        let kf = world.save_keyframe(String::new());

        // Reset offsets to 0.
        world.trace_offsets.clear();

        // Load keyframe — offset should be restored to 1.
        world.load_keyframe(&kf);

        // Draining again should still be empty (offset preserved).
        let still_empty = world.drain_new_traces();
        assert!(still_empty.is_empty());
    }

    #[test]
    fn test_pause_stops_run() {
        let mut world = World::new();

        let mut m = Machine::with_defaults(0, "m0");
        // Schedule events at time 10, 20, 30.
        m.schedule_at(10, 0, "e1", Box::new(|_| {}));
        m.schedule_at(20, 0, "e2", Box::new(|_| {}));
        m.schedule_at(30, 0, "e3", Box::new(|_| {}));
        world.add_machine(m);

        // Pause then run — the loop exits immediately because !running.
        world.pause();
        world.run().unwrap();

        // World should still be at time 0 because loop never entered.
        assert_eq!(world.now, 0);

        // Resume and run.
        world.resume();
        world.run().unwrap();
        assert_eq!(world.now, 30);
    }

    // ── CAN isolation tests ────────────────────────────────────────────

    use std::sync::{Arc, Mutex};

    #[test]
    fn test_paused_world_drive_world_returns_paused() {
        let mut world = World::new();
        let mut m = Machine::with_defaults(0, "m0");
        m.schedule_at(10, 0, "e1", Box::new(|_| {}));
        world.add_machine(m);

        world.pause();
        let outcome = crate::control::drive_world(&mut world, crate::control::RunLimit::ToCompletion);
        assert!(matches!(outcome.termination, crate::control::RunTermination::Paused));
        assert_eq!(world.now, 0);

        // Resume and drive — should complete now.
        world.resume();
        let outcome = crate::control::drive_world(&mut world, crate::control::RunLimit::ToCompletion);
        assert!(matches!(outcome.termination, crate::control::RunTermination::Complete));
        assert_eq!(world.now, 10);
    }

    #[test]
    fn test_stopped_world_drive_world_returns_stopped() {
        let mut world = World::new();
        let mut m = Machine::with_defaults(0, "m0");
        m.schedule_at(10, 0, "e1", Box::new(|_| {}));
        world.add_machine(m);

        world.stop();
        let outcome = crate::control::drive_world(&mut world, crate::control::RunLimit::ToCompletion);
        assert!(matches!(outcome.termination, crate::control::RunTermination::Stopped));
        assert_eq!(world.now, 0);
        assert!(world.is_stopped());
    }

    #[test]
    fn test_resumed_paused_world_can_continue() {
        let mut world = World::new();
        let mut m = Machine::with_defaults(0, "m0");
        m.schedule_at(10, 0, "e1", Box::new(|_| {}));
        m.schedule_at(20, 0, "e2", Box::new(|_| {}));
        world.add_machine(m);

        // Run first event.
        let outcome = crate::control::drive_world(&mut world, crate::control::RunLimit::EventCount(1));
        assert!(matches!(outcome.termination, crate::control::RunTermination::LimitReached));
        assert_eq!(world.now, 10);

        // Pause, then resume, then run to completion.
        world.pause();
        let outcome = crate::control::drive_world(&mut world, crate::control::RunLimit::ToCompletion);
        assert!(matches!(outcome.termination, crate::control::RunTermination::Paused));
        assert_eq!(world.now, 10); // didn't advance

        world.resume();
        let outcome = crate::control::drive_world(&mut world, crate::control::RunLimit::ToCompletion);
        assert!(matches!(outcome.termination, crate::control::RunTermination::Complete));
        assert_eq!(world.now, 20);
    }

    /// Shared test state for CAN isolation verification.
    #[derive(Default, Clone)]
    struct CanTestState {
        step_count: Arc<Mutex<u32>>,
        sent_frames: Arc<Mutex<Vec<u32>>>,
        recv_frames: Arc<Mutex<Vec<u32>>>,
    }

    /// A firmware that sends/receives CAN frames and records activity
    /// in shared state so the test can inspect it.
    struct CanTestFirmware {
        machine_id: u64,
        state: CanTestState,
    }

    impl CanTestFirmware {
        fn new(machine_id: u64) -> Self {
            Self { machine_id, state: CanTestState::default() }
        }

        fn state(&self) -> &CanTestState {
            &self.state
        }
    }

    impl Firmware for CanTestFirmware {
        fn init(&mut self, machine: &mut Machine) {
            machine.with_device_context(|| {
                sim_devices::can_insert(sim_devices::VirtualCan::new(0, 500_000));
            });
            // Schedule an event so the World has something to process
            // and will call step_firmware.
            machine.schedule_at(0, 100, "can_test_boot", Box::new(|_| {}));
        }

        fn step(&mut self, _now: Tick, machine: &mut Machine) {
            let mut count = self.state.step_count.lock().unwrap();
            *count += 1;
            let step = *count;
            drop(count);

            // Use the full device context for CAN operations.
            let _guard = machine.activate();

            // Drain received frames.
            sim_devices::with_can_mut(0, |can| {
                while let Some(frame) = can.recv() {
                    self.state.recv_frames.lock().unwrap().push(frame.id);
                }
            });

            // Send a unique frame each step.
            let frame_id = (self.machine_id as u32) * 1000 + step;
            sim_devices::with_can_mut(0, |can| {
                can.send(CanFrame::new_data(frame_id, &[self.machine_id as u8]));
            });
            self.state.sent_frames.lock().unwrap().push(frame_id);
        }
    }

    #[test]
    fn two_machines_owned_can_dont_cross_observe() {
        let mut world = World::new();

        let mut m1 = Machine::with_defaults(1, "m1");
        let mut m2 = Machine::with_defaults(2, "m2");
        m1.enable_owned_bank();
        m2.enable_owned_bank();

        let fw1 = CanTestFirmware::new(1);
        let fw2 = CanTestFirmware::new(2);
        let s1 = fw1.state().clone();
        let s2 = fw2.state().clone();

        // Init must happen after setting firmware, so we do it manually.
        {
            let mut fw1_init = CanTestFirmware { machine_id: 1, state: s1.clone() };
            let mut fw2_init = CanTestFirmware { machine_id: 2, state: s2.clone() };
            fw1_init.init(&mut m1);
            fw2_init.init(&mut m2);
        }
        m1.set_firmware(Box::new(fw1));
        m2.set_firmware(Box::new(fw2));

        world.add_machine(m1);
        world.add_machine(m2);
        world.owned_banks_enabled = true;

        // Run a few steps.
        for _ in 0..5 {
            let _ = world.step();
        }

        let s1_sent: Vec<u32> = s1.sent_frames.lock().unwrap().clone();
        let s1_recv: Vec<u32> = s1.recv_frames.lock().unwrap().clone();
        let s2_sent: Vec<u32> = s2.sent_frames.lock().unwrap().clone();
        let s2_recv: Vec<u32> = s2.recv_frames.lock().unwrap().clone();

        assert!(!s1_sent.is_empty(), "machine 1 should have sent frames");
        assert!(!s2_sent.is_empty(), "machine 2 should have sent frames");

        // Machine 1 must NOT observe machine 2's frame IDs.
        for &f2_id in &s2_sent {
            assert!(
                !s1_recv.contains(&f2_id),
                "machine 1 must not observe machine 2's frame {f2_id}"
            );
        }
        // Machine 2 must NOT observe machine 1's frame IDs.
        for &f1_id in &s1_sent {
            assert!(
                !s2_recv.contains(&f1_id),
                "machine 2 must not observe machine 1's frame {f1_id}"
            );
        }
    }

    #[test]
    fn owned_can_drains_tx_from_only_firmware_step() {
        let mut world = World::new();
        let mut m = Machine::with_defaults(1, "m1");
        m.enable_owned_bank();

        let fw = CanTestFirmware::new(1);
        let s = fw.state().clone();

        {
            let mut fw_init = CanTestFirmware { machine_id: 1, state: s.clone() };
            fw_init.init(&mut m);
        }
        m.set_firmware(Box::new(fw));

        world.add_machine(m);
        world.owned_banks_enabled = true;

        // Run one step — firmware should step and CAN TX should be drained.
        world.step().unwrap();

        // After step, CAN TX queue should be empty (fully drained).
        let m = world.machines.get(&1).unwrap();
        m.with_device_context(|| {
            let remaining = sim_devices::with_can_mut(0, |can| can.tx_queue.len());
            assert_eq!(remaining, Some(0), "CAN TX queue must be empty after firmware step drain");
        });

        assert!(*s.step_count.lock().unwrap() > 0, "firmware must have stepped");
    }

    #[test]
    fn can_tx_drain_does_not_strand_frames() {
        let mut world = World::new();
        world.add_bus(CanBus::new("vcan0", 500));

        let mut m1 = Machine::with_defaults(1, "sender");
        let mut m2 = Machine::with_defaults(2, "receiver");
        m1.enable_owned_bank();
        m2.enable_owned_bank();

        let fw1 = CanTestFirmware::new(1);
        let s1 = fw1.state().clone();

        {
            let mut fw_init = CanTestFirmware { machine_id: 1, state: s1.clone() };
            fw_init.init(&mut m1);
        }
        m1.set_firmware(Box::new(fw1));
        m2.set_firmware(Box::new(CanTestFirmware::new(2)));

        {
            let bus = world.bus_mut("vcan0").unwrap();
            bus.attach(1);
            bus.attach(2);
        }
        world.add_machine(m1);
        world.add_machine(m2);
        world.owned_banks_enabled = true;

        // Run for a few steps.
        for _ in 0..3 {
            let _ = world.step();
        }

        let s1_sent = s1.sent_frames.lock().unwrap();
        assert!(!s1_sent.is_empty(), "sender must have sent frames");

        // The TX queue should be empty — frames were drained.
        let sender = world.machines.get(&1).unwrap();
        sender.with_device_context(|| {
            let tx = sim_devices::with_can_mut(0, |can| can.tx_queue.len());
            assert_eq!(tx, Some(0), "CAN TX queue must be empty after each step");
        });
    }

    // ── Two-World isolation tests ──────────────────────────────────────

    #[test]
    fn two_worlds_owned_can_interleave_100x() {
        // Build solo World A and record its full trace for comparison.
        let mut solo_a = World::new();
        let mut ma = Machine::with_defaults(1, "a");
        ma.enable_owned_bank();
        let fw_a = CanTestFirmware::new(1);
        let sa_solo = fw_a.state().clone();
        {
            let mut init = CanTestFirmware { machine_id: 1, state: sa_solo.clone() };
            init.init(&mut ma);
        }
        ma.set_firmware(Box::new(fw_a));
        solo_a.add_machine(ma);
        solo_a.owned_banks_enabled = true;
        for _ in 0..10 { let _ = solo_a.step(); }
        let solo_a_sent: Vec<u32> = sa_solo.sent_frames.lock().unwrap().clone();
        let solo_a_trace = solo_a.drain_all_traces();

        // Build solo World B and record its full trace for comparison.
        let mut solo_b = World::new();
        let mut mb = Machine::with_defaults(1, "b");
        mb.enable_owned_bank();
        let fw_b = CanTestFirmware::new(2);
        let sb_solo = fw_b.state().clone();
        {
            let mut init = CanTestFirmware { machine_id: 2, state: sb_solo.clone() };
            init.init(&mut mb);
        }
        mb.set_firmware(Box::new(fw_b));
        solo_b.add_machine(mb);
        solo_b.owned_banks_enabled = true;
        for _ in 0..10 { let _ = solo_b.step(); }
        let solo_b_sent: Vec<u32> = sb_solo.sent_frames.lock().unwrap().clone();
        let solo_b_trace = solo_b.drain_all_traces();

        // Now run the interleaved test 100 times.
        for _ in 0..100 {
            // World A
            let mut world_a = World::new();
            let mut ma = Machine::with_defaults(1, "a");
            ma.enable_owned_bank();
            let fw_a = CanTestFirmware::new(1);
            let sa = fw_a.state().clone();
            {
                let mut init = CanTestFirmware { machine_id: 1, state: sa.clone() };
                init.init(&mut ma);
            }
            ma.set_firmware(Box::new(fw_a));
            world_a.add_machine(ma);
            world_a.owned_banks_enabled = true;

            // World B
            let mut world_b = World::new();
            let mut mb = Machine::with_defaults(1, "b");
            mb.enable_owned_bank();
            let fw_b = CanTestFirmware::new(2);
            let sb = fw_b.state().clone();
            {
                let mut init = CanTestFirmware { machine_id: 2, state: sb.clone() };
                init.init(&mut mb);
            }
            mb.set_firmware(Box::new(fw_b));
            world_b.add_machine(mb);
            world_b.owned_banks_enabled = true;

            // Step A then B (10 steps each).
            for _ in 0..10 {
                let _ = world_a.step();
                let _ = world_b.step();
            }

            // Verify A trace equals solo A trace.
            let a_trace = world_a.drain_all_traces();
            assert_eq!(a_trace, solo_a_trace, "interleaved A trace must equal solo A trace");

            // Verify B trace equals solo B trace.
            let b_trace = world_b.drain_all_traces();
            assert_eq!(b_trace, solo_b_trace, "interleaved B trace must equal solo B trace");

            // Verify frame isolation.
            let a_sent: Vec<u32> = sa.sent_frames.lock().unwrap().clone();
            let b_sent: Vec<u32> = sb.sent_frames.lock().unwrap().clone();
            let a_recv: Vec<u32> = sa.recv_frames.lock().unwrap().clone();
            let b_recv: Vec<u32> = sb.recv_frames.lock().unwrap().clone();

            // A must have sent its own frames.
            assert_eq!(a_sent, solo_a_sent, "interleaved A must send same frames as solo A");
            assert_eq!(b_sent, solo_b_sent, "interleaved B must send same frames as solo B");

            // World A must not see World B's frames.
            for &f in &b_sent {
                assert!(!a_recv.contains(&f), "world A observed world B frame {f}");
            }
            // World B must not see World A's frames.
            for &f in &a_sent {
                assert!(!b_recv.contains(&f), "world B observed world A frame {f}");
            }
        }
    }

    // ── Restart tests ──────────────────────────────────────────────────

    #[test]
    fn reboot_no_other_event_reaches_boot() {
        let mut world = World::new();
        let mut m = Machine::with_defaults(1, "test");
        m.enable_owned_bank();

        let fw = CanTestFirmware::new(1);
        let s = fw.state().clone();
        {
            let mut init = CanTestFirmware { machine_id: 1, state: s.clone() };
            init.init(&mut m);
        }
        m.set_firmware(Box::new(fw));

        // Set a factory so restart can reconstruct.
        let factory: FirmwareFactory = std::sync::Arc::new(move || {
            Box::new(CanTestFirmware::new(1))
        });
        m.set_firmware_factory(factory);

        world.add_machine(m);
        world.owned_banks_enabled = true;

        // Reboot with downtime.
        let reboot = FaultAction::Reboot { machine_id: 1, downtime_ms: 5 };
        reboot.apply(&mut world, 1000); // 1ms in

        // At this point, no machines have events (the old one was removed,
        // the new one is stopped).  Only pending_boots keeps the world alive.
        // run() should advance to boot_at and reconstruct the machine.
        world.run().unwrap();

        // The machine should exist again.
        assert!(world.machines.contains_key(&1), "machine should be reconstructed after reboot");

        // Verify machine_reset_boot marker exists (recorded on the
        // reconstructed machine). machine_reset_begin was on the old
        // machine which was destroyed; tracking it requires a world-level
        // event log (out of scope for this pass).
        let traces = world.drain_all_traces();
        let has_boot = traces.iter().any(|l| l.contains("machine_reset_boot"));
        assert!(has_boot, "must have machine_reset_boot");
    }

    #[test]
    fn restart_preserves_persistent_and_resets_volatile() {
        let mut world = World::new();
        let mut m = Machine::with_defaults(1, "test");
        m.enable_owned_bank();

        // Write a value to flash (persistent).
        m.with_device_context(|| {
            sim_devices::flash_insert(sim_devices::VirtualFlash::new(0));
            sim_devices::with_flash_mut(0, |flash| {
                flash.write_page(0, 0, &[0xAA, 0xBB, 0xCC, 0xDD]);
            });
        });

        // Queue a CAN frame (volatile).
        m.with_device_context(|| {
            sim_devices::can_insert(sim_devices::VirtualCan::new(0, 500_000));
            sim_devices::with_can_mut(0, |can| {
                can.send(CanFrame::new_data(0x100, &[1, 2, 3]));
            });
        });

        // Set factory.
        let factory: FirmwareFactory = std::sync::Arc::new(|| Box::new(CanTestFirmware::new(1)));
        m.set_firmware_factory(factory);

        world.add_machine(m);
        world.owned_banks_enabled = true;

        // Reboot.
        let reboot = FaultAction::Reboot { machine_id: 1, downtime_ms: 0 };
        reboot.apply(&mut world, 1000);
        world.run().unwrap();

        let m = world.machines.get(&1).unwrap();

        // Flash should be preserved.
        m.with_device_context(|| {
            let byte0 = sim_devices::with_flash(0, |flash| flash.read(0));
            assert_eq!(byte0, Some(Some(0xAA)), "flash byte 0 must survive restart");
        });

        // CAN TX queue should be gone (volatile reset).
        m.with_device_context(|| {
            let tx_len = sim_devices::with_can_mut(0, |can| can.tx_queue.len());
            assert_eq!(tx_len, Some(0), "CAN TX must be reset on restart");
        });
    }

    #[test]
    fn restart_downtime_delivery_boundary() {
        use std::sync::{Arc, Mutex};

        /// Shared state for recording received CAN frames post-reboot.
        struct DeliveryState {
            recv: Mutex<Vec<u32>>,
        }
        impl DeliveryState {
            fn new() -> Arc<Self> {
                Arc::new(Self { recv: Mutex::new(Vec::new()) })
            }
        }

        struct DeliveryFirmware {
            state: Arc<DeliveryState>,
        }
        impl Firmware for DeliveryFirmware {
            fn init(&mut self, machine: &mut Machine) {
                machine.with_device_context(|| {
                    sim_devices::can_insert(sim_devices::VirtualCan::new(0, 500_000));
                });
            }
            fn step(&mut self, _now: Tick, machine: &mut Machine) {
                let _guard = machine.activate();
                sim_devices::with_can_mut(0, |can| {
                    while let Some(frame) = can.recv() {
                        self.state.recv.lock().unwrap().push(frame.id);
                    }
                });
            }
        }

        let mut world = World::new();
        world.add_bus(CanBus::new("vcan0", 0)); // zero latency
        world.owned_banks_enabled = true;

        // Machine 0: idle sender (no firmware).
        let mut m0 = Machine::with_defaults(0, "sender");
        m0.enable_owned_bank();
        world.add_machine(m0);

        // Machine 1: receiver — will be rebooted.
        let mut m1 = Machine::with_defaults(1, "receiver");
        m1.enable_owned_bank();

        let state = DeliveryState::new();
        let state_clone = state.clone();
        m1.set_firmware_factory(Arc::new(move || {
            Box::new(DeliveryFirmware { state: state_clone.clone() })
        }));
        m1.load_firmware(Box::new(DeliveryFirmware { state: state.clone() }));
        world.add_machine(m1);

        // Attach both machines to the bus.
        {
            let bus = world.bus_mut("vcan0").unwrap();
            bus.attach(0);
            bus.attach(1);
        }

        // Reboot machine 1 at tick 1000.  downtime_ms=5 → boot_at = 6000.
        let reboot = FaultAction::Reboot { machine_id: 1, downtime_ms: 5 };
        reboot.apply(&mut world, 1000);

        // Queue two CAN frames from m0 → m1:
        //   frame 0x100 sent at 2000 — arrives during downtime → MUST be dropped.
        //   frame 0x200 sent at 6000 — arrives exactly at boot_at → MUST be received
        //     (process_pending_boots runs before deliver_buses).
        {
            let bus = world.bus_mut("vcan0").unwrap();
            bus.send(0, 0x100, &[1], 2000);
            bus.send(0, 0x200, &[2], 6000);
        }

        world.run().unwrap();

        let recv = state.recv.lock().unwrap();
        assert!(
            !recv.contains(&0x100),
            "frame 0x100 sent during downtime must NOT be received (arrives before boot_at)"
        );
        assert!(
            recv.contains(&0x200),
            "frame 0x200 sent at boot_at must be received (boot before bus delivery)"
        );
    }
}

