//! A simulated machine — a self-contained simulator instance with its
//! own event queue, fiber runtime, and trace sink.
//!
//! Each [`Machine`] wraps a [`Simulator`] from `sim-ffi` and adds
//! multi-machine awareness: machine ID tagging on trace events and
//! the ability to query the next scheduled event time for global
//! time coordination by the [`World`](super::World).

use sim_core::event_queue::EventCallback;
use sim_core::{SimConfig, SimError, Tick, TraceEvent, TraceSink};
use sim_ffi::simulator::Simulator;
use sim_ffi::TaskContext;
use sim_fiber::TaskId;

use crate::firmware::Firmware;

/// A self-contained simulated machine.
///
/// Each machine has its own:
/// - event queue (deterministic min-heap)
/// - fiber runtime (for Rust and C tasks)
/// - trace sink (prefixed with machine ID)
/// - device inventory
/// - RTOS backend selector ("freertos" or "zephyr")
///
/// The [`World`](super::World) coordinates multiple machines by
/// querying their next event times and advancing them in lockstep.
pub struct Machine {
    /// Unique machine identifier within a World.
    pub id: u64,

    /// Human-readable machine name.
    pub name: String,

    /// RTOS backend: "freertos" (default) or "zephyr".
    /// Mixed-RTOS scenarios can assign different backends per machine.
    pub rtos: String,

    /// The underlying single-machine simulator.
    simulator: Simulator,

    /// Optional guest firmware loaded onto this machine.
    pub firmware: Option<Box<dyn Firmware>>,
}

impl Machine {
    /// Create a new machine with the given ID, name, and configuration.
    ///
    /// The machine gets its own trace sink and event queue.
    pub fn new(id: u64, name: &str, config: SimConfig) -> Self {
        let simulator = Simulator::new(config);
        Self {
            id,
            name: name.to_string(),
            rtos: "freertos".to_string(),
            simulator,
            firmware: None,
        }
    }

    /// Create a new machine with default configuration.
    pub fn with_defaults(id: u64, name: &str) -> Self {
        Self::new(id, name, SimConfig::default())
    }

    /// Create a new machine with a specific RTOS backend.
    pub fn with_rtos(id: u64, name: &str, rtos: &str) -> Self {
        let mut machine = Self::with_defaults(id, name);
        machine.rtos = rtos.to_string();
        machine
    }

    /// Spawn a native Rust task on this machine's fiber runtime.
    ///
    /// This is the multi-machine equivalent of
    /// [`sim_ffi::spawn_rust_task`].  The task runs on a stackful
    /// coroutine inside this machine's fiber pool.
    pub fn spawn_rust_task<F>(
        &mut self,
        name: &'static str,
        priority: u32,
        stack_size: usize,
        f: F,
    ) -> TaskId
    where
        F: FnOnce(TaskContext) + Send + 'static,
    {
        self.simulator
            .spawn_rust_task(name, priority, stack_size, f)
    }

    /// Schedule a callback on this machine's event queue at the
    /// given absolute virtual time.
    pub fn schedule_at(
        &mut self,
        at: Tick,
        priority: u16,
        label: &'static str,
        callback: EventCallback,
    ) -> u64 {
        self.simulator.schedule_at(at, priority, label, callback)
    }

    /// Record a trace event directly on this machine's trace sink.
    ///
    /// Used by the World to record link-delivery events (PacketRx)
    /// and other cross-machine interactions.
    pub fn record_trace(&mut self, event: TraceEvent) {
        self.simulator.record_trace(event);
    }

    /// Return the virtual time of the next pending event, or `None`
    /// if the machine is idle (no events, all tasks exited/blocked).
    pub fn next_event_time(&self) -> Option<Tick> {
        self.simulator.peek_time()
    }

    /// Advance this machine's simulation until the given deadline.
    ///
    /// All events with `at ≤ deadline` are dispatched.  After this
    /// call, `self.now()` will be at most `deadline`.
    ///
    /// If firmware is loaded, its [`Firmware::step`] is called at the
    /// deadline so it can react to incoming messages and schedule new
    /// work.  Firmware is temporarily taken out during the step to
    /// avoid borrow conflicts.
    pub fn advance_to(&mut self, deadline: Tick) -> Result<(), SimError> {
        // Only advance if there are events to process and the deadline
        // hasn't already passed.
        if self.next_event_time().is_none_or(|t| t > deadline) {
            return Ok(());
        }

        self.simulator.run_until(deadline)?;

        // After the simulator advances, give firmware a chance to
        // react to the new virtual time.
        if let Some(mut fw) = self.firmware.take() {
            fw.step(deadline, self);
            self.firmware = Some(fw);
        }

        Ok(())
    }

    /// Return the current virtual time of this machine.
    pub fn now(&self) -> Tick {
        self.simulator.now()
    }

    /// Return true if this machine has no pending events and all
    /// tasks have exited or are blocked forever.
    ///
    /// If firmware is loaded, the machine is never considered idle
    /// — the firmware's RTOS scheduler manages task state outside
    /// the event queue.
    pub fn is_idle(&self) -> bool {
        if self.firmware.is_some() {
            return false;
        }
        self.simulator.is_idle()
    }

    /// Return a reference to this machine's trace sink.
    pub fn trace(&self) -> &TraceSink {
        self.simulator.trace()
    }

    /// Drain all trace events from this machine, prefixed with the
    /// machine ID.  Returns events ready for display.
    ///
    /// Merges events from both the World trace sink (event queue, CanBus,
    /// plant) and the firmware trace sink (FreeRTOS task events) if
    /// firmware is loaded.
    pub fn drain_trace_prefixed(&self) -> Vec<String> {
        let prefix = format!("[machine.{}]", self.id);

        let mut all: Vec<String> = self
            .trace()
            .events()
            .iter()
            .map(|e| format!("{} {}", prefix, e))
            .collect();

        // If firmware is loaded, also drain firmware trace events
        // (FreeRTOS task resume/yield/sleep, sim_trace_u32 calls, etc.)
        let fw_events: Vec<sim_core::TraceEvent> = self
            .simulator
            .sim_global
            .borrow()
            .trace
            .as_ref()
            .map(|t| t.events().to_vec())
            .unwrap_or_default();
        for e in &fw_events {
            all.push(format!("{} {}", prefix, e));
        }

        all
    }

    /// Load firmware onto this machine.
    ///
    /// Calls [`Firmware::init`] immediately so the firmware can
    /// schedule startup tasks and configure the machine.
    pub fn load_firmware(&mut self, mut firmware: Box<dyn Firmware>) {
        firmware.init(self);
        self.firmware = Some(firmware);
    }

    /// Remove and return the firmware from this machine, leaving
    /// `None` in its place.
    ///
    /// Used by [`World`](super::World) to temporarily take ownership
    /// of firmware during the step cycle.
    pub fn take_firmware(&mut self) -> Option<Box<dyn Firmware>> {
        self.firmware.take()
    }

    /// Set the firmware on this machine directly.
    ///
    /// Does NOT call [`Firmware::init`] — use [`load_firmware`](Self::load_firmware)
    /// for first-time loading.
    pub fn set_firmware(&mut self, firmware: Box<dyn Firmware>) {
        self.firmware = Some(firmware);
    }

    /// Return `true` if this machine has firmware loaded.
    pub fn has_firmware(&self) -> bool {
        self.firmware.is_some()
    }

    /// Activate this machine's simulator, making its SimGlobal the
    /// target for C ABI functions.  Returns a guard that deactivates
    /// on drop.
    ///
    /// Use this when calling C firmware functions (e.g., microcar_boot
    /// or sim_scheduler_tick) that need access to this machine's
    /// FreeRTOS task state.
    pub fn activate(&mut self) -> sim_ffi::simulator::SimulatorActivation<'_> {
        self.simulator.activate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_machine_create() {
        let machine = Machine::with_defaults(0, "test-machine");
        assert_eq!(machine.id, 0);
        assert_eq!(machine.name, "test-machine");
        assert_eq!(machine.rtos, "freertos");
        assert!(machine.is_idle());
        assert_eq!(machine.next_event_time(), None);
    }

    #[test]
    fn test_machine_with_rtos() {
        let machine = Machine::with_rtos(1, "zephyr-node", "zephyr");
        assert_eq!(machine.id, 1);
        assert_eq!(machine.rtos, "zephyr");
    }

    #[test]
    fn test_machine_schedule_and_advance() {
        let mut machine = Machine::with_defaults(1, "m1");

        // Schedule an event at time 10.
        machine.schedule_at(10, 0, "test-event", Box::new(|_ctx| {}));

        assert_eq!(machine.next_event_time(), Some(10));
        assert!(!machine.is_idle());

        // Advance to time 10 — the event fires.
        machine.advance_to(10).unwrap();
        assert_eq!(machine.now(), 10);
        assert!(machine.is_idle());
        assert_eq!(machine.next_event_time(), None);
    }

    #[test]
    fn test_machine_advance_to_partial() {
        let mut machine = Machine::with_defaults(2, "m2");

        machine.schedule_at(5, 0, "early", Box::new(|_| {}));
        machine.schedule_at(10, 0, "late", Box::new(|_| {}));

        // Advance to 7 — the first event fires (at 5), and the
        // simulator advances its clock to the deadline (7).
        machine.advance_to(7).unwrap();
        assert_eq!(machine.now(), 7);
        assert!(!machine.is_idle());
        assert_eq!(machine.next_event_time(), Some(10));

        // Advance to 15 — the second event fires (at 10), and the
        // simulator advances its clock to the deadline (15).
        machine.advance_to(15).unwrap();
        assert_eq!(machine.now(), 15);
        assert!(machine.is_idle());
    }

    #[test]
    fn test_machine_advance_to_empty() {
        let mut machine = Machine::with_defaults(3, "m3");
        // Advancing an idle machine should be a no-op.
        machine.advance_to(100).unwrap();
        assert_eq!(machine.now(), 0);
    }

    #[test]
    fn test_machine_spawn_rust_task() {
        let mut machine = Machine::with_defaults(4, "m4");
        let task_id = machine.spawn_rust_task("test-task", 1, 4096, |ctx| {
            ctx.sleep_for(5);
        });
        assert!(task_id > 0);

        // The task runs on a fiber — the simulator's event queue is
        // empty (fibers are managed separately), so is_idle() returns
        // true.  The task is still registered, just not in the queue.
        assert_eq!(machine.trace().len(), 0);
    }

    #[test]
    fn test_machine_record_trace() {
        let mut machine = Machine::with_defaults(5, "m5");
        machine.record_trace(TraceEvent::PacketRx { at: 10, len: 42 });

        let traces = machine.drain_trace_prefixed();
        assert_eq!(traces.len(), 1);
        assert!(traces[0].contains("[machine.5]"));
        assert!(traces[0].contains("pkt-rx"));
        assert!(traces[0].contains("42"));
    }
}
