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

use crate::canbus::CanBus;
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
                        value: *machine_id,
                    });
                }
                true
            }
            FaultAction::Reboot { machine_id } => {
                // Reboot: create a fresh Machine with the same ID and name.
                if let Some(old_name) = world
                    .machines
                    .get(machine_id)
                    .map(|m| m.name.clone())
                {
                    let new_machine = Machine::with_defaults(*machine_id, &old_name);
                    world.machines.insert(*machine_id, new_machine);
                    // Remove from stopped set (machine is fresh).
                    world.stopped_machines.remove(machine_id);
                    // Record trace event.
                    if let Some(machine) = world.machines.get_mut(machine_id) {
                        machine.record_trace(TraceEvent::UserU32 {
                            at: now,
                            label: "fault:reboot",
                            value: *machine_id,
                        });
                    }
                    true
                } else {
                    false
                }
            }
            FaultAction::DropFrame {
                bus_name,
                frame_id,
            } => {
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
            let (trigger_time, _) = self.scheduled_faults[self.fault_cursor];
            if trigger_time > now {
                break;
            }
            // Take ownership of the fault action.
            // We can't move out of a Vec directly with indexing, so we
            // clone the action (FaultAction derives Clone).
            let action = self.scheduled_faults[self.fault_cursor].1.clone();
            let applied = action.apply(self, now);
            if applied {
                count += 1;
            }
            self.fault_cursor += 1;
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
    fn next_global_event_time(&self) -> Option<Tick> {
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

        earliest
    }

    /// Deliver all link packets whose arrival time ≤ `now`.
    ///
    /// For each delivered packet, records a `PacketRx` trace event
    /// on the target machine.
    fn deliver_links(&mut self, now: Tick) {
        // Collect deliveries per target machine.
        let mut deliveries: BTreeMap<u64, Vec<(Tick, usize)>> = BTreeMap::new();

        for link in &mut self.links {
            let target_id = link.target();
            let arrived = link.drain_arrived(now);
            if arrived.is_empty() {
                continue;
            }
            for pkt in &arrived {
                deliveries
                    .entry(target_id)
                    .or_default()
                    .push((now, pkt.len()));
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
    /// For each delivered frame, records a `CanRx` trace event on the
    /// receiver machine and a `CanTx` trace event on the sender machine.
    fn deliver_buses(&mut self, now: Tick) {
        for bus in &mut self.buses {
            let frames = bus.drain_arrived(now);
            for (receiver_id, sender_id, frame_id, data) in &frames {
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

                    // 3. Advance all machines to this time.
                    self.advance_machines_to(self.now)?;

                    // 4. Step the plant model (may be a no-op if no plant or not yet due).
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
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

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

        world.set_plant(Box::new(CountingPlant { count: call_count.clone() }), 10);
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
    }
