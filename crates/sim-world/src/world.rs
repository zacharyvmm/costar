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
    /// Reboot a machine (destroy and recreate state, cold boot).
    Reboot {
        /// Machine ID to reboot.
        machine_id: u64,
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
            FaultAction::Reboot { machine_id } => {
                // Reboot: create a fresh Machine with the same ID and name.
                if let Some(old_name) = world.machines.get(machine_id).map(|m| m.name.as_str()) {
                    let new_machine = Machine::with_defaults(*machine_id, old_name);
                    world.machines.insert(*machine_id, new_machine);
                    // Remove from stopped set (machine is fresh).
                    world.stopped_machines.remove(machine_id);
                    // Record trace event.
                    if let Some(machine) = world.machines.get_mut(machine_id) {
                        machine.record_trace(TraceEvent::UserU32 {
                            at: now,
                            label: "fault:reboot",
                            value: *machine_id as u32,
                        });
                    }
                    true
                } else {
                    false
                }
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
    running: bool,
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
            trace_offsets: BTreeMap::new(),
            running: true,
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
                // ── Inject into firmware CAN RX queue (controller 0) ──
                // All firmware ECUs share CAN controller 0.  Each ECU's
                // firmware filters incoming frames by sender node ID
                // (first byte of data), so injecting everything into
                // controller 0 is safe — each ECU ignores frames not
                // addressed to it.
                let can_frame = CanFrame::new_data(*frame_id, data);
                sim_devices::with_can_mut(0, |can| can.inject_rx(can_frame));

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

        // Step each firmware with its host machine.
        for (id, mut fw) in firmwares.drain(..) {
            if let Some(machine) = self.machines.get_mut(&id) {
                fw.step(now, machine);

                // ── Bridge CAN TX: drain firmware CAN sends → World CanBus ──
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
                            // Bus isolation: a frame is only placed on the buses
                            // this machine is actually attached to. A machine on
                            // multiple buses (e.g. a gateway) sends on each of
                            // them, which is the intended multi-interface
                            // behavior; a machine on one bus cannot leak frames
                            // onto buses it is not a node of.
                            for bus in &mut self.buses {
                                if bus.nodes().contains(&id) {
                                    bus.send(id, f.id, payload, now);
                                }
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

    /// Run the simulation until all machines are idle and all links
    /// are empty, or until [`stop`](Self::stop) is called.
    ///
    /// If a plant model is attached, the loop continues stepping the
    /// plant even when machines and links are idle.
    pub fn run(&mut self) -> Result<(), SimError> {
        while self.running {
            let next_time = self.next_global_event_time();
            match next_time {
                Some(t) => {
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

                    // 2. Deliver bus frames at this time.
                    self.deliver_buses(self.now);

                    // 3. Apply scheduled faults.
                    self.apply_scheduled_faults(self.now);

                    // 3.1. Apply scheduled BLE injections.
                    self.apply_scheduled_ble_injections(self.now);

                    // 3.5. Step firmware on all machines.
                    self.step_firmware(self.now);

                    // 4. Advance all machines to this time.
                    self.advance_machines_to(self.now)?;

                    // 5. Step the plant model (may be a no-op if no plant or not yet due).
                    self.step_plant(self.now);

                    // 5. Check stop condition: all machines idle, links/buses
                    // empty, and no plant (plant keeps simulation alive).
                    if self.all_idle() && self.plant.is_none() {
                        break;
                    }
                }
                None => {
                    // No events anywhere — done.
                    break;
                }
            }
        }

        Ok(())
    }

    /// Run the simulation until the given deadline, all machines are
    /// idle, or [`stop`](Self::stop) is called.
    pub fn run_until(&mut self, deadline: Tick) -> Result<(), SimError> {
        while self.running && self.now < deadline {
            let next_time = self.next_global_event_time();
            match next_time {
                Some(t) if t <= deadline => {
                    if t < self.now {
                        return Err(SimError::TimeWentBackwards {
                            now: self.now,
                            event_at: t,
                        });
                    }

                    self.now = t;
                    self.deliver_links(self.now);
                    self.deliver_buses(self.now);
                    self.apply_scheduled_faults(self.now);
                    self.apply_scheduled_ble_injections(self.now);
                    self.step_firmware(self.now);
                    self.advance_machines_to(self.now)?;
                    self.step_plant(self.now);

                    if self.all_idle() && self.plant.is_none() {
                        break;
                    }
                }
                _ => {
                    // No events within the deadline window.
                    break;
                }
            }
        }

        Ok(())
    }

    /// Stop the simulation at the next iteration boundary.
    pub fn stop(&mut self) {
        self.running = false;
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
        self.running = false;
    }

    /// Resume the simulation after a pause.
    pub fn resume(&mut self) {
        self.running = true;
    }

    /// Return true if the simulation is paused.
    pub fn is_paused(&self) -> bool {
        !self.running
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
    fn test_firmware_can_tx_respects_bus_membership() {
        // A firmware CAN send must only be placed on the buses the sending
        // machine is actually attached to — it must not leak onto other buses.
        use crate::firmware::Firmware;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        // A CAN controller (id 0) must exist for a firmware send to route.
        sim_devices::can_insert(sim_devices::VirtualCan::new(0, 500_000));

        // Firmware that emits exactly one CAN frame (id 0x7A0) on first step.
        struct OneShotCanTx {
            sent: Arc<AtomicBool>,
        }
        impl Firmware for OneShotCanTx {
            fn step(&mut self, _now: Tick, _machine: &mut Machine) {
                if !self.sent.swap(true, Ordering::SeqCst) {
                    sim_devices::with_can_mut(0, |can| {
                        can.tx_queue.push(CanFrame::new_data(0x7A0, &[1, 2, 3]));
                    });
                }
            }
        }

        let mut world = World::new();

        // sender(1) + same_bus(2) on busA; other_bus(3) on busB only.
        let mut sender = Machine::with_defaults(1, "sender");
        sender.schedule_at(10, 0, "kick", Box::new(|_| {}));
        sender.load_firmware(Box::new(OneShotCanTx {
            sent: Arc::new(AtomicBool::new(false)),
        }));
        world.add_machine(sender);
        world.add_machine(Machine::with_defaults(2, "same_bus"));
        world.add_machine(Machine::with_defaults(3, "other_bus"));

        let mut bus_a = CanBus::new("busA", 100);
        bus_a.attach(1);
        bus_a.attach(2);
        world.add_bus(bus_a);

        let mut bus_b = CanBus::new("busB", 100);
        bus_b.attach(3);
        world.add_bus(bus_b);

        world.run_until(1000).unwrap();

        let trace = world.drain_all_traces();
        let got_on_bus_a = trace
            .iter()
            .any(|l| l.contains("can-rx") && l.contains("receiver=2") && l.contains("0x07a0"));
        let leaked_to_bus_b = trace
            .iter()
            .any(|l| l.contains("can-rx") && l.contains("receiver=3") && l.contains("0x07a0"));

        assert!(
            got_on_bus_a,
            "same-bus node (2) must receive the frame; trace: {trace:?}"
        );
        assert!(
            !leaked_to_bus_b,
            "frame must NOT leak to a node (3) on a bus the sender is not attached to; trace: {trace:?}"
        );
    }

    #[test]
    fn test_pause_resume() {
        let mut world = World::new();
        assert!(!world.is_paused());
        world.pause();
        assert!(world.is_paused());
        world.resume();
        assert!(!world.is_paused());
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
}
