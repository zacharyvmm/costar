//! Simulator — the public Rust API surface.
//!
//! Combines the deterministic event queue (`SimulatorCore`) with the
//! fiber runtime into a single API matching HANDOFF §14.
//!
//! # Example
//!
//! ```ignore
//! let mut sim = Simulator::new(SimConfig::default());
//!
//! // Schedule a virtual event.
//! sim.schedule_at(100, 10, "timer_fire", |ctx| {
//!     println!("timer fired at {}", ctx.now);
//! });
//!
//! // Spawn a Rust task (runs on the same fiber runtime as C tasks).
//! sim.spawn_rust_task("blinker", 1, 4096, |ctx| {
//!     ctx.sleep_for(10);
//! });
//!
//! // Run until all events are consumed.
//! sim.run();
//! ```

use sim_core::{
    event_queue::{EventCallback, EventId},
    run_loop::{SimulatorContext, SimulatorCore},
    time::Tick,
    trace::TraceSink,
    SimConfig, SimResult,
};

use sim_fiber::TaskId;

use crate::TaskContext;
use crate::{activate_sim_global, SimGlobal, SimGlobalGuard};

use sim_devices::{activate_bank, BankGuard, DeviceBank};

/// The top-level simulator.
///
/// Owns the event queue, trace sink, FreeRTOS state (tasks, etc.), and
/// provides methods for scheduling events, spawning Rust tasks, and running
/// the simulation.
///
/// Each `Simulator` has its own isolated `SimGlobal` — C ABI functions
/// operate on whichever simulator is currently active via
/// [`activate()`](Simulator::activate).
pub struct Simulator {
    core: SimulatorCore,
    ctx: SimulatorContext,
    /// Per-simulator FreeRTOS state (tasks, next task ID, interrupt state).
    /// This is what C ABI functions find when this simulator is active.
    pub sim_global: std::cell::RefCell<SimGlobal>,
    /// Optional per-simulator device bank.
    ///
    /// `None` (the default) means device C-ABI accessors resolve into the
    /// process/thread-default bank exactly as before — so every existing
    /// single-World scenario is byte-identical.  When a caller opts in via
    /// [`enable_owned_devices`](Simulator::enable_owned_devices), the simulator
    /// owns its own [`DeviceBank`] and [`activate`](Simulator::activate) scopes
    /// it alongside `SimGlobal`, so two execution contexts using the same device
    /// ids (e.g. CAN controller 0) no longer collide.  This is the
    /// device-ownership slice of the per-World execution-context guard
    /// (`UNBLOCKING.md` P0a migration step 3); clock/task-identity are
    /// deliberately not moved here.
    owned_devices: Option<DeviceBank>,
}

impl Simulator {
    /// Create a new simulator with the given configuration.
    pub fn new(config: SimConfig) -> Self {
        let mut core = SimulatorCore::new(config);
        // Safety: core.trace is pinned in the struct, and the raw pointer
        // is only used within the single-threaded run loop.
        let trace_ptr: *mut TraceSink = &mut core.trace;
        let ctx = SimulatorContext::new(trace_ptr);
        let mut sim_global = SimGlobal::new();
        // Initialise the per-Simulator firmware trace sink so that
        // sim_trace_u32, sim_can_send/sim_can_recv, and other C ABI
        // trace calls have somewhere to write.  drain_trace_prefixed()
        // merges this with the World trace (SimulatorCore.trace).
        sim_global.trace = Some(Box::new(TraceSink::new()));
        let sim_global = std::cell::RefCell::new(sim_global);
        Self {
            core,
            ctx,
            sim_global,
            owned_devices: None,
        }
    }

    /// Give this simulator its own [`DeviceBank`] so that
    /// [`activate`](Simulator::activate) scopes device state to this simulator.
    ///
    /// Idempotent: calling it again keeps the existing bank (and its devices).
    /// Opt-in — a simulator that never calls this uses the shared default bank
    /// and is byte-identical to the previous behavior.
    pub fn enable_owned_devices(&mut self) {
        if self.owned_devices.is_none() {
            self.owned_devices = Some(DeviceBank::new());
        }
    }

    /// Whether this simulator owns its own device bank.
    pub fn owns_devices(&self) -> bool {
        self.owned_devices.is_some()
    }

    /// Activate this simulator — make its `SimGlobal` available to C ABI
    /// functions called from this thread.
    ///
    /// Returns a guard that deactivates on drop.  While the guard is alive,
    /// all `sim_*` C ABI functions (`sim_create_task`, `sim_start_scheduler`,
    /// etc.) operate on this simulator's state.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut sim = Simulator::new(SimConfig::default());
    /// {
    ///     let _guard = sim.activate();
    ///     // C ABI functions now use sim's state.
    /// }
    /// // C ABI functions revert to the previous state.
    /// ```
    pub fn activate(&mut self) -> SimulatorActivation<'_> {
        // Activate SimGlobal (FreeRTOS/task state) for C ABI calls, and — when
        // this simulator owns a device bank — the DeviceBank too, so device
        // C-ABI accessors resolve into this simulator's devices.  Both guards
        // restore the previous context on drop, including on panic unwind.
        let sim_guard = activate_sim_global(&self.sim_global);
        let bank_guard = self.owned_devices.as_ref().map(activate_bank);
        SimulatorActivation {
            _guard: sim_guard,
            _bank_guard: bank_guard,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Schedule a callback at an absolute virtual timestamp.
    ///
    /// Lower `priority` values run first (0 = fatal, 10 = IRQ, 20 = tick,
    /// 100 = background).  Returns an opaque ID that can be used to cancel
    /// the event.
    pub fn schedule_at(
        &mut self,
        at: Tick,
        priority: u16,
        label: &'static str,
        callback: EventCallback,
    ) -> EventId {
        self.core.schedule_at(at, priority, label, callback)
    }

    /// Schedule a callback `delta` ticks from the current virtual time.
    pub fn schedule_after(
        &mut self,
        delta: Tick,
        priority: u16,
        label: &'static str,
        callback: EventCallback,
    ) -> EventId {
        self.core.schedule_after(delta, priority, label, callback)
    }

    /// Cancel a previously scheduled event.  Returns true if it existed.
    pub fn cancel(&mut self, id: EventId) -> bool {
        self.core.cancel(id)
    }

    /// Spawn a Rust task on the shared fiber runtime.
    ///
    /// The closure `f` executes as the task body inside a stackful coroutine.
    /// It receives a [`TaskContext`] for yield/sleep/time operations.
    ///
    /// Tasks created this way coexist with C FreeRTOS tasks managed through
    /// the `sim_abi.h` interface.
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
        crate::spawn_rust_task(name, priority, stack_size, f)
    }

    /// Run the event loop until no events remain or a stop condition is met.
    ///
    /// Events are dispatched in virtual-time order.  After each event, the
    /// guest RTOS scheduler is drained (a no-op by default; overridden when
    /// the C-driven fiber scheduler runs).
    pub fn run(&mut self) -> SimResult<()> {
        self.core.run(&mut self.ctx)
    }

    /// Run until an absolute virtual timestamp.
    ///
    /// Events at or before `deadline` are dispatched; the simulation pauses
    /// at `deadline` and can be resumed later.
    pub fn run_until(&mut self, deadline: Tick) -> SimResult<()> {
        self.core.run_until(&mut self.ctx, deadline)
    }

    /// Run until no events remain (alias for `run`).
    pub fn run_until_idle(&mut self) -> SimResult<()> {
        self.core.run_until_idle(&mut self.ctx)
    }

    /// Stop the run loop at the next iteration.
    pub fn stop(&mut self) {
        self.core.stop();
    }

    /// Access the trace sink (read-only).
    pub fn trace(&self) -> &TraceSink {
        &self.core.trace
    }

    /// Current virtual time.
    pub fn now(&self) -> Tick {
        self.core.now
    }

    /// Whether the event queue is empty.
    pub fn is_idle(&self) -> bool {
        self.core.queue.is_empty()
    }

    /// Peek at the next event's virtual time without popping it.
    pub fn peek_time(&self) -> Option<Tick> {
        self.core.queue.peek_time()
    }

    /// Record a trace event directly on this simulator's trace sink.
    ///
    /// Used by the multi-machine World to record cross-machine events
    /// (e.g. PacketRx from link deliveries).
    pub fn record_trace(&mut self, event: sim_core::trace::TraceEvent) {
        self.core.trace.record(event);
    }
}

/// RAII guard that keeps a `Simulator` active on the current thread.
///
/// Created by [`Simulator::activate()`].  While this guard is alive,
/// all C ABI functions (`sim_*`) operate on the associated simulator.
/// When dropped, the previous simulator (or none) is restored.
pub struct SimulatorActivation<'a> {
    /// The actual activation guard (holds the old pointer).
    _guard: SimGlobalGuard,
    /// Device-bank guard, present only when the simulator owns a device bank.
    /// Restores the previously active bank on drop.
    _bank_guard: Option<BankGuard<'a>>,
    /// Phantom lifetime to tie the guard to the simulator borrow.
    _phantom: std::marker::PhantomData<&'a mut ()>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simulator_schedule_and_run() {
        let mut sim = Simulator::new(SimConfig::default());

        // Schedule two events.
        sim.schedule_at(100, 10, "late", Box::new(|_| {}));
        sim.schedule_at(50, 10, "early", Box::new(|_| {}));

        sim.run().unwrap();

        assert_eq!(sim.now(), 100);
        assert!(sim.is_idle());
        assert!(sim.trace().len() >= 4); // 2 schedule + 2 dispatch
    }

    #[test]
    fn test_simulator_run_until() {
        let mut sim = Simulator::new(SimConfig::default());

        sim.schedule_at(100, 10, "a", Box::new(|_| {}));
        sim.schedule_at(200, 10, "b", Box::new(|_| {}));
        sim.schedule_at(300, 10, "c", Box::new(|_| {}));

        sim.run_until(150).unwrap();
        assert_eq!(sim.now(), 150);
        assert!(!sim.is_idle()); // events b and c remain

        sim.run_until(500).unwrap();
        assert_eq!(sim.now(), 500);
        assert!(sim.is_idle());
    }

    #[test]
    fn test_simulator_cancel() {
        let mut sim = Simulator::new(SimConfig::default());

        let id = sim.schedule_at(100, 10, "cancelled", Box::new(|_| {}));
        sim.schedule_at(200, 10, "fires", Box::new(|_| {}));
        assert!(sim.cancel(id));

        sim.run().unwrap();
        assert_eq!(sim.now(), 200);

        // Only the "fires" event and its schedule should be in the trace.
        let events: Vec<_> = sim
            .trace()
            .events()
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    sim_core::trace::TraceEvent::EventDispatched { .. }
                        | sim_core::trace::TraceEvent::EventScheduled { .. }
                )
            })
            .collect();
        // 3: "cancelled" scheduled, "fires" scheduled + dispatched
        // + 1 cancellation event
        assert!(events.len() >= 3, "got {} events", events.len());
    }

    #[test]
    fn test_simulator_time_rollback_rejected() {
        let mut sim = Simulator::new(SimConfig::default());

        // Manually set time forward.
        sim.core.now = 500;
        sim.ctx.now = 500;

        sim.schedule_at(100, 10, "past", Box::new(|_| {}));
        let result = sim.run();
        assert!(result.is_err());
    }

    #[test]
    fn test_simulator_stop() {
        let mut sim = Simulator::new(SimConfig::default());

        sim.schedule_at(100, 10, "first", Box::new(|_| {}));
        sim.schedule_at(500, 10, "stopper", {
            // We can't capture &sim directly, so use a static flag.
            Box::new(|_ctx| {
                // Just record that this callback fired.
            })
        });
        sim.schedule_at(1000, 10, "never_reached", Box::new(|_| {}));

        // Run until 500, then stop.
        sim.run_until(500).unwrap();
        sim.stop();

        assert_eq!(sim.now(), 500);
    }

    // ── Execution-context guard: DeviceBank scoping (P0a migration step 3) ──

    use sim_devices::{with_can, with_can_mut, CanFrame, VirtualCan};

    /// A simulator that owns its devices scopes them through `activate()`:
    /// two such simulators each using CAN controller id 0 do not cross-observe.
    #[test]
    fn owned_devices_two_simulators_isolate_can_id_zero() {
        let mut a = Simulator::new(SimConfig::default());
        let mut b = Simulator::new(SimConfig::default());
        a.enable_owned_devices();
        b.enable_owned_devices();
        assert!(a.owns_devices() && b.owns_devices());

        // Simulator A: controller 0 sends frame 0xA1.
        {
            let _act = a.activate();
            sim_devices::can_insert(VirtualCan::new(0, 500_000));
            assert!(with_can_mut(0, |c| c.send(CanFrame::new_data(0xA1, &[1]))).unwrap());
        }
        // Simulator B: controller 0 sends a different frame 0xB2.
        {
            let _act = b.activate();
            sim_devices::can_insert(VirtualCan::new(0, 500_000));
            assert!(with_can_mut(0, |c| c.send(CanFrame::new_data(0xB2, &[2]))).unwrap());
            assert_eq!(with_can(0, |c| c.tx_queue.len()).unwrap(), 1);
        }
        // Back in A: its controller 0 still holds only its own frame.
        {
            let _act = a.activate();
            let (len, id) = with_can(0, |c| (c.tx_queue.len(), c.tx_queue[0].id)).unwrap();
            assert_eq!(len, 1, "A must be untouched by B");
            assert_eq!(id, 0xA1, "A must see its own frame, not B's");
        }
    }

    /// Nested activation restores the outer simulator's device context when the
    /// inner guard drops.
    #[test]
    fn owned_devices_nested_activation_restores_outer() {
        let mut outer = Simulator::new(SimConfig::default());
        let mut inner = Simulator::new(SimConfig::default());
        outer.enable_owned_devices();
        inner.enable_owned_devices();

        let _o = outer.activate();
        sim_devices::can_insert(VirtualCan::new(0, 500_000));
        {
            let _i = inner.activate();
            assert!(
                with_can(0, |_| ()).is_none(),
                "inner bank has no controller 0"
            );
        }
        // Inner dropped: outer's controller 0 is visible again.
        assert!(with_can(0, |_| ()).is_some(), "outer controller 0 restored");
    }

    /// A panic while an owned-device simulator is active restores the previous
    /// device context (so a sibling execution context still works).
    #[test]
    fn owned_devices_panic_restores_context() {
        // A controller id unique to this test, only ever inserted into the
        // panicking simulator's OWNED bank — so its visibility after the unwind
        // is a robust signal of whether the active-context pointer was restored,
        // independent of any default-bank state left by sibling tests.
        const UNIQUE_ID: u32 = 0x7EAD;
        let mut sim = Simulator::new(SimConfig::default());
        sim.enable_owned_devices();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _act = sim.activate();
            sim_devices::can_insert(VirtualCan::new(UNIQUE_ID, 500_000));
            assert!(with_can(UNIQUE_ID, |_| ()).is_some());
            panic!("boom while active");
        }));
        assert!(result.is_err());
        // After unwind the guard dropped: the owned bank is no longer active, so
        // its unique controller is not visible from the restored context.
        assert!(
            with_can(UNIQUE_ID, |_| ()).is_none(),
            "panic must restore the previous device context (owned bank deactivated)"
        );
        // And the simulator can be re-activated to reach its bank again.
        let _act = sim.activate();
        assert!(
            with_can(UNIQUE_ID, |_| ()).is_some(),
            "the owned bank itself survived; re-activation reaches it"
        );
    }

    /// A freshly-created simulator does not own devices — documenting that the
    /// production default keeps using the shared bank and stays byte-identical.
    #[test]
    fn default_simulator_does_not_own_devices() {
        let sim = Simulator::new(SimConfig::default());
        assert!(!sim.owns_devices());
    }
}
