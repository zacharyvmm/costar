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
use sim_devices::bank::{activate_bank, BankGuard, DeviceBank};
use std::cell::RefCell;
use std::rc::Rc;

use sim_fiber::TaskId;

use crate::guest_runtime::{activate_guest_runtime, GuestRuntime, GuestRuntimeGuard};
use crate::TaskContext;
use crate::{activate_sim_global, SimGlobal, SimGlobalGuard};

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
    pub sim_global: Rc<RefCell<SimGlobal>>,
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

    /// Per-simulator guest runtime: `sim_instance_state` regions + retained
    /// clock/task identity (Stage B1). Activated alongside `SimGlobal` and the
    /// `DeviceBank`; a fresh `Simulator` (as after restart) gets fresh regions.
    guest_runtime: Rc<GuestRuntime>,
}

/// Cloneable, thread-local execution context for one [`Simulator`].
///
/// A context owns handles to the simulator state it activates.  That lets a
/// caller obtain the context before borrowing another part of its `Machine`,
/// then execute firmware inside [`with_active`](Self::with_active) without a
/// long-lived borrow of the `Simulator`.  Contexts are intentionally
/// thread-affine because the simulator and device state use `RefCell`.
#[derive(Clone)]
// TODO(Stage B3): Wire NetworkBank into this context so each machine's
// network context is activated alongside SimGlobal, DeviceBank, and GuestRuntime.
// Currently `sim_net::with_eth_device_mut(0, …)` falls back to the global
// thread-local store, which means two Worlds sharing a thread can observe each
// other's network device state. See `sim-net/src/bank.rs` for the existing
// NetworkBank infrastructure.
pub struct SimulatorExecutionContext {
    sim_global: Rc<RefCell<SimGlobal>>,
    device_bank: Option<DeviceBank>,
    guest_runtime: Rc<GuestRuntime>,
}

impl SimulatorExecutionContext {
    /// Run `f` with this simulator's C ABI state active on the current thread.
    ///
    /// The `SimGlobal` and optional `DeviceBank` are activated and restored in
    /// one lexical scope, including panic unwind.  This is the production API
    /// for firmware execution and host-side device operations.
    pub fn with_active<R>(&self, f: impl FnOnce() -> R) -> R {
        let _activation = self.activate();
        f()
    }

    fn activate(&self) -> ActiveSimulatorContext {
        // Keep the device activation nested inside the SimGlobal activation so
        // both are restored in reverse dependency order on drop.
        let sim_global_guard = activate_sim_global(&self.sim_global);
        let device_bank_guard = self.device_bank.as_ref().map(activate_bank);
        let guest_runtime_guard = activate_guest_runtime(&self.guest_runtime);
        ActiveSimulatorContext {
            _guest_runtime_guard: guest_runtime_guard,
            _device_bank_guard: device_bank_guard,
            _sim_global_guard: sim_global_guard,
        }
    }
}

/// Guard backing the closure-scoped and compatibility activation APIs.
///
/// The guards themselves own their active stack entries, so this type never
/// relies on a raw pointer or a borrowed context remaining live.
struct ActiveSimulatorContext {
    // Field order is drop order.  Deactivate devices before the associated
    // simulator global so nested C ABI dispatch is restored coherently.
    _guest_runtime_guard: GuestRuntimeGuard,
    _device_bank_guard: Option<BankGuard>,
    _sim_global_guard: SimGlobalGuard,
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
            sim_global: Rc::new(sim_global),
            owned_devices: None,
            guest_runtime: Rc::new(GuestRuntime::new()),
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

    /// Return a cloneable execution context for this simulator.
    ///
    /// The returned context owns the handles needed to activate this
    /// simulator, rather than borrowing `self`.  A `Machine` can therefore
    /// obtain it before passing `&mut self` to firmware code.
    pub fn execution_context(&self) -> SimulatorExecutionContext {
        SimulatorExecutionContext {
            sim_global: self.sim_global.clone(),
            device_bank: self.owned_devices.clone(),
            guest_runtime: self.guest_runtime.clone(),
        }
    }

    /// Run `f` with this simulator's C ABI and device context active.
    ///
    /// Prefer this closure-scoped API for new code.  It cannot be dropped out
    /// of order or forgotten by ordinary callers, and it restores the prior
    /// context on panic unwind.
    pub fn with_active_context<R>(&self, f: impl FnOnce() -> R) -> R {
        self.execution_context().with_active(f)
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
        // Activate all three execution contexts: SimGlobal (C ABI), DeviceBank
        // (per-machine virtual device isolation), and GuestRuntime (per-machine
        // clock/task identity / sim_instance_state).  This is the full
        // activation that `SimulatorExecutionContext::with_active` provides.
        // Previously this path only activated SimGlobal, which meant firmware
        // boot and scheduler ticks ran with no device isolation (default bank),
        // breaking the per-machine ownership contract.
        let sim_global_guard = activate_sim_global(&self.sim_global);
        let device_bank_guard = self.owned_devices.as_ref().map(activate_bank);
        let guest_runtime_guard = activate_guest_runtime(&self.guest_runtime);
        SimulatorActivation {
            _guest_runtime_guard: Some(guest_runtime_guard),
            _device_bank_guard: device_bank_guard,
            _sim_global_guard: sim_global_guard,
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
    _guest_runtime_guard: Option<GuestRuntimeGuard>,
    /// Device bank guard (restores prior bank on drop).
    _device_bank_guard: Option<BankGuard>,
    /// SimGlobal guard (restores prior C ABI state on drop).
    _sim_global_guard: SimGlobalGuard,
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
}
