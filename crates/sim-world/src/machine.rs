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

use crate::board::{BoardConfig, BoardError};
use crate::firmware::{Firmware, FirmwareFactory};

/// A self-contained simulated machine.
///
/// Each machine has its own:
/// - event queue (deterministic min-heap)
/// - fiber runtime (for Rust and C tasks)
/// - trace sink (prefixed with machine ID)
/// - device inventory
/// - RTOS backend selector
///
/// The [`World`](super::World) coordinates multiple machines by
/// querying their next event times and advancing them in lockstep.
pub struct Machine {
    /// Unique machine identifier within a World.
    pub id: u64,

    /// Human-readable machine name.
    pub name: String,

    /// RTOS backend: FreeRTOS (default) or Zephyr.
    /// Mixed-RTOS scenarios can assign different backends per machine.
    pub rtos: crate::RtosBackend,

    /// The underlying single-machine simulator.
    simulator: Simulator,

    /// Optional guest firmware loaded onto this machine.
    pub firmware: Option<Box<dyn Firmware>>,

    /// Optional factory that reconstructs this machine's firmware from scratch.
    /// When set, a restart recreates the firmware (and runs its boot path via
    /// [`Firmware::init`]) instead of leaving a bare machine. `Arc` so it
    /// survives the `Machine` being replaced on restart.
    firmware_factory: Option<FirmwareFactory>,

    /// The board definition currently configured on this machine.  Stored as a
    /// cloneable [`BoardConfig`] so a restart can recreate the same peripherals.
    board: BoardConfig,

    /// The immutable [`SimConfig`] this machine was created with. Retained so a
    /// restart can reconstruct the machine from its immutable specification.
    config: SimConfig,

    /// Next World-time wakeup demanded by sleeping FreeRTOS fibers, if any.
    ///
    /// FreeRTOS delays use a per-simulator tick timeline. After each firmware
    /// step the World converts the earliest fiber sleep deadline into an
    /// absolute World timestamp so `next_global_event_time` keeps pumping the
    /// machine while tasks are blocked in `vTaskDelay` / `vTaskDelayUntil`.
    firmware_next_world_wake: Option<Tick>,

    /// `(World time, FreeRTOS tick)` of the first firmware step: maps World
    /// microseconds to firmware ticks so firmware time follows the World
    /// clock exactly and never runs ahead of it.
    firmware_clock_anchor: Option<(Tick, Tick)>,

    /// IRQs raised with [`raise_irq`](Self::raise_irq) before the first
    /// firmware step fixed [`firmware_clock_anchor`](Self::firmware_clock_anchor),
    /// as `(irq, World arrival time)`.  Converted to firmware ticks once the
    /// anchor exists, so they use the same clock mapping as everything else.
    irqs_before_anchor: Vec<(u32, Tick)>,
}

impl Machine {
    /// Create a new machine with the given ID, name, and configuration.
    ///
    /// The machine gets its own trace sink and event queue.
    ///
    /// By default, device access uses the thread-local default bank for
    /// backward compatibility (byte-identical golden traces).  Call
    /// [`World::enable_owned_device_banks`](super::World::enable_owned_device_banks)
    /// to give every machine its own [`DeviceBank`](sim_devices::DeviceBank)
    /// so that CAN controller 0 and other virtual devices are scoped
    /// per-machine rather than shared (UNBLOCKING.md B1).
    pub fn new(id: u64, name: &str, config: SimConfig) -> Self {
        let simulator = Simulator::new(config);
        Self {
            config,
            id,
            name: name.to_string(),
            rtos: crate::RtosBackend::default(),
            simulator,
            firmware: None,
            firmware_factory: None,
            board: BoardConfig::default(),
            firmware_next_world_wake: None,
            firmware_clock_anchor: None,
            irqs_before_anchor: Vec::new(),
        }
    }

    /// Create a new machine with default configuration.
    pub fn with_defaults(id: u64, name: &str) -> Self {
        Self::new(id, name, SimConfig::default())
    }

    /// Create a new machine with a specific RTOS backend.
    pub fn with_rtos(id: u64, name: &str, rtos: crate::RtosBackend) -> Self {
        let mut machine = Self::with_defaults(id, name);
        machine.rtos = rtos;
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
        match (self.simulator.peek_time(), self.firmware_next_world_wake) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Record the next World-time wakeup required by sleeping FreeRTOS fibers.
    ///
    /// `world_now` is the World timestamp at which firmware was just stepped.
    /// FreeRTOS scheduler ticks are 1 ms (`configTICK_RATE_HZ == 1000`), so each
    /// FreeRTOS tick maps to 1000 World microseconds.
    ///
    /// Call this after a firmware step even if `firmware` was temporarily taken
    /// out of the machine (as `World::step_firmware` does).
    pub fn refresh_firmware_wake_from_fibers(&mut self, world_now: Tick) {
        // A stopped machine (an interrupt storm, `vTaskEndScheduler()`,
        // another fatal kernel state) never runs firmware again, whatever
        // its backend: no firmware wake, though its task table may still
        // hold ready or sleeping tasks.  Independent machine events (the
        // event queue) are kept by `next_event_time`.
        if self.simulator.halted() {
            self.firmware_next_world_wake = None;
            return;
        }
        if self.simulator.runs_freertos() {
            // Everything the firmware still has to do, including work that
            // appeared after the scheduler ran in this step; `None` once the
            // scheduler has ended.  Independent machine events (the event
            // queue) are kept by `next_event_time` either way.
            self.firmware_next_world_wake = self
                .simulator
                .freertos_pending_work_tick()
                .map(|tick| self.world_wake(tick, world_now));
            return;
        }

        let sim_now = self.simulator.scheduler_sim_time();
        // IRQ input staged with `raise_irq` (or by device code with
        // `raise_at`): one already due that can be taken wakes the machine
        // at once, a later one at its arrival.
        // Peripheral callbacks (`sim_schedule_event`) likewise: one due now
        // wakes the machine at once, a later one at its deadline.
        let (irq_due, next_irq, next_callback, next_timer) = self.with_device_context(|| {
            let (irq_due, next_irq) = sim_devices::irq::with_irq(|c| {
                (
                    !sim_ffi::is_critical_locked() && c.first_due(sim_now).is_some(),
                    c.next_arrival_after(sim_now),
                )
            });
            (
                irq_due,
                next_irq,
                sim_ffi::next_event_deadline(),
                sim_devices::next_timer_expiry(),
            )
        });
        let callback_due = next_callback.is_some_and(|at| at <= sim_now);
        // A virtual timer expired (armed after the scheduler ran): it fires,
        // and latches its IRQ, at the next step.
        let timer_due = next_timer.is_some_and(|at| at <= sim_now);
        if irq_due || callback_due || timer_due || self.simulator.has_fresh_runnable_fiber() {
            // Input or a callback due now, or a task that is ready and has
            // not run yet (new, or just woken): step the machine again at
            // once.
            self.firmware_next_world_wake = Some(world_now.saturating_add(1));
            return;
        }
        // A task that ran and is still runnable (a busy one, or one
        // yielding while it waits for an ISR) is stepped at every World
        // event and, failing any, at the next firmware tick, where its
        // clock moves on (see `Simulator::catch_up_to_limit`): time passes
        // for a busy machine, and the World never busy-wakes it within one
        // tick.  Earlier deadlines below still win.
        let runnable_wake = self
            .simulator
            .has_runnable_fiber()
            .then(|| sim_now.saturating_add(1));
        // The next sleeper, callback or IRQ arrival, at its absolute World
        // time under the machine's one firmware clock mapping.
        let next = [
            self.simulator.earliest_fiber_sleep_until(),
            next_callback,
            next_irq,
            next_timer,
            runnable_wake,
        ]
        .into_iter()
        .flatten()
        .filter(|&tick| tick > sim_now)
        .min();
        self.firmware_next_world_wake = next.map(|tick| self.world_wake(tick, world_now));
    }

    /// The World time to wake this machine for firmware work due at `tick`:
    /// the tick's World time under the firmware clock anchor (fixed at the
    /// first firmware step, see [`firmware_tick_to_world`](Self::firmware_tick_to_world)),
    /// never before the next World instant.  Every firmware deadline the
    /// World wakes a machine for goes through here, whatever the backend.
    fn world_wake(&mut self, tick: Tick, world_now: Tick) -> Tick {
        self.firmware_clock_anchor(world_now);
        self.firmware_tick_to_world(tick)
            .max(world_now.saturating_add(1))
    }

    /// Bound the next firmware step to World time `world_now`.
    ///
    /// Call before `Firmware::step`.  FreeRTOS firmware then runs every task
    /// due up to the FreeRTOS tick matching `world_now` and stops there.
    ///
    /// The FreeRTOS kernel is brought up to that tick here (see
    /// [`Simulator::catch_up_to_limit`](sim_ffi::simulator::Simulator::catch_up_to_limit)),
    /// so host code in `Firmware::step` that acts on the firmware before
    /// running the scheduler (resuming a task, giving a semaphore, ...) acts
    /// at World time `world_now`, not at the machine's previous tick.
    pub fn begin_firmware_step(&mut self, world_now: Tick) {
        let (anchor_world, anchor_tick) = self.firmware_clock_anchor(world_now);
        let elapsed_ticks = world_now.saturating_sub(anchor_world) / Self::us_per_freertos_tick();
        self.simulator
            .set_scheduler_limit(Some(anchor_tick + elapsed_ticks));
        self.simulator.catch_up_to_limit();
    }

    /// Raise interrupt `irq` from outside the firmware, arriving at World
    /// time `world_at` (now or later).
    ///
    /// Firmware time is tick-granular: the arrival is converted to the
    /// first firmware tick at or after `world_at` (never an earlier one, so
    /// the ISR cannot run before the input exists).  The ISR runs at that
    /// tick even if the firmware is still at an earlier tick or has
    /// interrupts masked when its next step starts.  The machine is woken
    /// at the World time of that tick.  Input raised before the machine's
    /// first firmware step is converted once that step fixes the mapping
    /// from World time to firmware ticks.
    pub fn raise_irq(&mut self, irq: u32, world_at: Tick) {
        // A stopped machine takes no input and is never woken for it.
        if self.simulator.halted() {
            return;
        }
        let wake = match self.firmware_clock_anchor {
            Some(anchor) => self.stage_irq(anchor, irq, world_at),
            None => {
                // The World-to-firmware mapping is fixed by the first
                // firmware step; convert then (see `firmware_clock_anchor`).
                self.irqs_before_anchor.push((irq, world_at));
                world_at
            }
        };
        self.firmware_next_world_wake = Some(
            self.firmware_next_world_wake
                .map_or(wake, |current| current.min(wake)),
        );
    }

    /// Stage IRQ `irq` for the first firmware tick at or after World time
    /// `world_at` under clock anchor `(anchor_world, anchor_tick)`.  Returns
    /// the World time of that tick.
    fn stage_irq(
        &self,
        (anchor_world, anchor_tick): (Tick, Tick),
        irq: u32,
        world_at: Tick,
    ) -> Tick {
        let us_per_tick = Self::us_per_freertos_tick();
        let elapsed_ticks = world_at.saturating_sub(anchor_world).div_ceil(us_per_tick);
        let at_tick = anchor_tick + elapsed_ticks;
        self.with_device_context(|| {
            sim_devices::irq::with_irq_mut(|c| c.raise_at(irq, at_tick));
        });
        anchor_world
            .saturating_add(elapsed_ticks.saturating_mul(us_per_tick))
            .max(world_at)
    }

    /// World microseconds per FreeRTOS tick.
    fn us_per_freertos_tick() -> u64 {
        (1_000_000 / u64::from(sim_ffi::freertos::tick_rate_hz().max(1))).max(1)
    }

    fn firmware_clock_anchor(&mut self, world_now: Tick) -> (Tick, Tick) {
        if let Some(anchor) = self.firmware_clock_anchor {
            return anchor;
        }
        let anchor = (world_now, self.simulator.scheduler_sim_time());
        self.firmware_clock_anchor = Some(anchor);
        for (irq, world_at) in std::mem::take(&mut self.irqs_before_anchor) {
            self.stage_irq(anchor, irq, world_at);
        }
        anchor
    }

    /// Advance this machine's simulation until the given deadline.
    ///
    /// All events with `at ≤ deadline` are dispatched.  After this
    /// call, `self.now()` will be at most `deadline`.
    ///
    /// **Owned-bank path (B1)**: firmware stepping and CAN TX draining are
    /// handled exclusively by [`World::step_firmware`](super::World::step_firmware).
    /// There is exactly one firmware-step/drain boundary per tick, so CAN
    /// frames generated during firmware execution are never stranded
    /// indefinitely (UNBLOCKING.md B2).
    ///
    /// **Legacy path (no owned banks)**: the extra `Firmware::step` is
    /// preserved for byte-identical golden traces.  CAN TX generated here
    /// is drained on the *next* tick's `step_firmware` pass — the legacy
    /// double-step behaviour.
    pub fn advance_to(&mut self, deadline: Tick) -> Result<(), SimError> {
        // Only advance if there are events to process and the deadline
        // hasn't already passed.
        if self.next_event_time().is_none_or(|t| t > deadline) {
            return Ok(());
        }

        self.simulator.run_until(deadline)?;

        // When owned device banks are enabled, World::step_firmware owns the
        // single firmware-step and CAN-drain boundary per tick. The legacy
        // path retains the extra step for byte-identical golden traces.
        if !self.simulator.owns_devices() {
            if let Some(mut fw) = self.firmware.take() {
                self.begin_firmware_step(deadline);
                fw.step(deadline, self);
                self.firmware = Some(fw);
                self.refresh_firmware_wake_from_fibers(deadline);
            }
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

    /// Convert a firmware (FreeRTOS tick) timestamp to World microseconds.
    ///
    /// Uses the anchor recorded at the first firmware step.  Before that
    /// step, the firmware clock is taken to start at this machine's current
    /// World time.
    pub fn firmware_tick_to_world(&self, tick: Tick) -> Tick {
        let (anchor_world, anchor_tick) = self
            .firmware_clock_anchor
            .unwrap_or((self.now(), self.simulator.scheduler_sim_time()));
        let us_per_tick = Self::us_per_freertos_tick();
        if tick >= anchor_tick {
            anchor_world.saturating_add((tick - anchor_tick).saturating_mul(us_per_tick))
        } else {
            anchor_world.saturating_sub((anchor_tick - tick).saturating_mul(us_per_tick))
        }
    }

    /// Drain all trace events from this machine, prefixed with the
    /// machine ID.  Returns events ready for display.
    ///
    /// Merges events from both the World trace sink (event queue, CanBus,
    /// plant) and the firmware trace sink (FreeRTOS task events) if
    /// firmware is loaded.  Firmware events are recorded in FreeRTOS ticks;
    /// they are converted to World microseconds here so every line shares
    /// one time domain.
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
        for e in fw_events {
            let e = e.map_time(|tick| self.firmware_tick_to_world(tick));
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

    /// Load firmware from a [`FirmwareFactory`], recording the factory so a
    /// later restart can recreate the firmware and run its boot path.
    ///
    /// Constructs a fresh firmware via the factory, then loads it (calling
    /// [`Firmware::init`], the boot path).
    pub fn load_firmware_from_factory(&mut self, factory: FirmwareFactory) {
        let firmware = factory();
        self.firmware_factory = Some(factory);
        self.load_firmware(firmware);
    }

    /// Set the firmware factory without (re)loading firmware now.
    pub fn set_firmware_factory(&mut self, factory: FirmwareFactory) {
        self.firmware_factory = Some(factory);
    }

    /// Whether this machine has a firmware factory (can be restarted).
    pub fn has_firmware_factory(&self) -> bool {
        self.firmware_factory.is_some()
    }

    /// Return a clone of this machine's firmware factory, if any.  The clone is
    /// cheap (`Arc`) and lets a restart move the factory onto a fresh machine.
    pub fn firmware_factory(&self) -> Option<FirmwareFactory> {
        self.firmware_factory.clone()
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

    /// Return a cloneable execution context for this machine's simulator.
    ///
    /// The returned [`SimulatorExecutionContext`] owns the handles needed to
    /// activate this machine's `SimGlobal` and [`DeviceBank`](sim_devices::DeviceBank)
    /// without borrowing the `Machine` itself.  A caller (e.g., the World's
    /// `step_firmware`) can clone it and activate the machine's context inside
    /// a closure via [`with_active`](SimulatorExecutionContext::with_active),
    /// ensuring that every `sim_devices::with_can_mut(0, …)` call during
    /// firmware execution resolves to *this* machine's private CAN controller
    /// rather than a shared default bank (B1, UNBLOCKING.md).
    pub fn execution_context(&self) -> sim_ffi::simulator::SimulatorExecutionContext {
        self.simulator.execution_context()
    }

    /// Run `f` with this machine's device context active.
    ///
    /// Delegates to the retained-owner
    /// [`SimulatorExecutionContext::with_active`](sim_ffi::simulator::SimulatorExecutionContext::with_active),
    /// so every `sim_devices::with_*` accessor invoked inside `f` resolves into
    /// *this* machine's private [`DeviceBank`](sim_devices::DeviceBank).
    pub fn with_device_context<R>(&self, f: impl FnOnce() -> R) -> R {
        self.simulator.with_active_context(f)
    }

    /// Replace the complete board definition, validate it, and initialize its
    /// devices inside this machine's device context.
    ///
    /// Returns the number of peripherals initialized. Partial/append
    /// configuration is not supported — the previous board definition (and the
    /// devices it created) is fully replaced.
    pub fn configure_board(&mut self, board: BoardConfig) -> Result<usize, BoardError> {
        board.validate()?;
        self.board = board;
        let count = self.with_device_context(|| self.board.initialize_devices());
        Ok(count)
    }

    /// The board definition currently configured on this machine.
    pub fn board_config(&self) -> &BoardConfig {
        &self.board
    }

    /// The immutable [`SimConfig`] this machine was created with.
    pub fn sim_config(&self) -> SimConfig {
        self.config
    }

    /// Snapshot this machine's persistent devices (flash/EEPROM/block) from its
    /// device context. Used by the World restart algorithm before removing the
    /// old machine.
    pub fn snapshot_persistent_devices(&self) -> sim_devices::PersistentDeviceState {
        self.with_device_context(sim_devices::snapshot_persistent_devices)
    }

    /// Restore persistent devices into this machine's device context, replacing
    /// its flash/EEPROM/block contents.
    pub fn restore_persistent_devices(&self, state: sim_devices::PersistentDeviceState) {
        self.with_device_context(|| sim_devices::restore_persistent_devices(state));
    }

    /// Give this machine its own device and network banks. After this call,
    /// firmware CAN TX/RX and Ethernet/TCP state resolve to the private banks
    /// instead of the thread-local default banks.  Called by
    /// [`World::enable_owned_device_banks`].
    pub(crate) fn enable_owned_bank(&mut self) {
        self.simulator.enable_owned_devices();
        self.simulator.enable_owned_network();
    }

    /// Ensure Ethernet device 0 exists in this machine's owned NetworkBank.
    ///
    /// Activates only the network bank (not FreeRTOS) so this is safe before
    /// firmware load / scheduler start.
    #[allow(dead_code)] // reserved for eth-link provisioning once gRPC+FreeRTOS is stable
    pub(crate) fn ensure_eth_device_zero(&self) {
        let mac = [0x02, 0x00, 0x00, 0x00, (self.id >> 8) as u8, self.id as u8];
        let _ = self.simulator.with_owned_network_bank(|| {
            if sim_net::with_eth_device(0, |_| ()).is_none() {
                sim_net::eth_device_insert(sim_net::VirtualEthDevice::new(0, mac, 1500));
            }
        });
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
        assert_eq!(machine.rtos, crate::RtosBackend::FreeRtos);
        assert!(machine.is_idle());
        assert_eq!(machine.next_event_time(), None);
    }

    #[test]
    fn test_machine_with_rtos() {
        let machine = Machine::with_rtos(1, "zephyr-node", crate::RtosBackend::Zephyr);
        assert_eq!(machine.id, 1);
        assert_eq!(machine.rtos, crate::RtosBackend::Zephyr);
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
