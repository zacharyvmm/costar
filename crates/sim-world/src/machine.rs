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

/// A self-contained simulated machine.
///
/// Each machine has its own:
/// - event queue (deterministic min-heap)
/// - fiber runtime (for Rust and C tasks)
/// - trace sink (prefixed with machine ID)
/// - device inventory
///
/// The [`World`](super::World) coordinates multiple machines by
/// querying their next event times and advancing them in lockstep.
pub struct Machine {
    /// Unique machine identifier within a World.
    pub id: u64,

    /// Human-readable machine name.
    pub name: String,

    /// The underlying single-machine simulator.
    simulator: Simulator,
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
            simulator,
        }
    }

    /// Create a new machine with default configuration.
    pub fn with_defaults(id: u64, name: &str) -> Self {
        Self::new(id, name, SimConfig::default())
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
    pub fn advance_to(&mut self, deadline: Tick) -> Result<(), SimError> {
        // Only advance if there are events to process and the deadline
        // hasn't already passed.
        if self.next_event_time().map_or(true, |t| t > deadline) {
            return Ok(());
        }

        self.simulator.run_until(deadline)
    }

    /// Return the current virtual time of this machine.
    pub fn now(&self) -> Tick {
        self.simulator.now()
    }

    /// Return true if this machine has no pending events and all
    /// tasks have exited or are blocked forever.
    pub fn is_idle(&self) -> bool {
        self.simulator.is_idle()
    }

    /// Return a reference to this machine's trace sink.
    pub fn trace(&self) -> &TraceSink {
        self.simulator.trace()
    }

    /// Drain all trace events from this machine, prefixed with the
    /// machine ID.  Returns events ready for display.
    pub fn drain_trace_prefixed(&self) -> Vec<String> {
        let prefix = format!("[machine.{}]", self.id);
        self.trace()
            .events()
            .iter()
            .map(|e| format!("{} {}", prefix, e))
            .collect()
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
        assert!(machine.is_idle());
        assert_eq!(machine.next_event_time(), None);
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
