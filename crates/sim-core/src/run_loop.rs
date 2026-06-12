//! Simulator run loop.
//!
//! The `SimulatorCore` owns virtual time, the event queue, the trace sink,
//! and the run loop.  It dispatches events in order and drains the guest
//! RTOS scheduler after each event.  The core never spawns host threads.

use crate::{
    config::SimConfig,
    error::{SimError, SimResult},
    event_queue::{EventCallback, EventContext, EventId, EventQueue},
    time::Tick,
    trace::TraceSink,
};

/// The concrete context passed to event callbacks and the RTOS drain.
///
/// This struct bundles the state that event callbacks and scheduler drains
/// need.  Real implementations will carry references to the fiber runtime
/// and RTOS port adapter.
pub struct SimulatorContext {
    /// Reference to the trace sink (owned by `SimulatorCore`).
    pub trace: *mut TraceSink,
    /// Current virtual time.
    pub now: Tick,
}

impl SimulatorContext {
    /// Create a new context.  `trace` must point to the `SimulatorCore`'s
    /// own trace sink.
    pub fn new(trace: *mut TraceSink) -> Self {
        Self { trace, now: 0 }
    }
}

// Safety: `SimulatorContext` is only accessed from the single-threaded run loop.
unsafe impl Send for SimulatorContext {}

impl EventContext for SimulatorContext {
    fn drain_rtos_scheduler(&mut self, _now: Tick) -> SimResult<()> {
        // Default implementation: no guest RTOS to drain.
        // Overridden when a fiber runtime is attached.
        Ok(())
    }
}

/// The simulator core.
pub struct SimulatorCore {
    /// Current virtual time.
    pub now: Tick,
    /// The deterministic event queue.
    pub queue: EventQueue,
    /// Whether the run loop is active.
    pub running: bool,
    /// The trace sink.
    pub trace: TraceSink,
    /// Configuration.
    pub config: SimConfig,
}

impl SimulatorCore {
    /// Create a new simulator core with the given configuration.
    pub fn new(config: SimConfig) -> Self {
        Self {
            now: 0,
            queue: EventQueue::new(),
            running: false,
            trace: TraceSink::new(),
            config,
        }
    }

    /// Schedule an event at an absolute virtual timestamp.
    pub fn schedule_at(
        &mut self,
        at: Tick,
        priority: u16,
        label: &'static str,
        callback: EventCallback,
    ) -> EventId {
        let id = self.queue.schedule_at(at, priority, label, callback);
        self.trace
            .event_scheduled(self.now, id, priority, label, at);
        id
    }

    /// Schedule an event `delta` ticks from the current virtual time.
    pub fn schedule_after(
        &mut self,
        delta: Tick,
        priority: u16,
        label: &'static str,
        callback: EventCallback,
    ) -> EventId {
        let at = self.now.saturating_add(delta);
        self.schedule_at(at, priority, label, callback)
    }

    /// Cancel a previously scheduled event.
    pub fn cancel(&mut self, id: EventId) -> bool {
        if self.queue.cancel(id) {
            self.trace.event_cancelled(self.now, id);
            true
        } else {
            false
        }
    }

    /// Run the event loop until no events remain or a stop condition is met.
    pub fn run(&mut self, ctx: &mut SimulatorContext) -> SimResult<()> {
        self.running = true;

        while self.running {
            let Some(event) = self.queue.pop_next() else {
                break;
            };

            if let Some(ref key) = event.key {
                if key.at < self.now {
                    return Err(SimError::TimeWentBackwards {
                        now: self.now,
                        event_at: key.at,
                    });
                }

                self.now = key.at;
                ctx.now = self.now;
                self.trace.event_dispatch(self.now, key.id, event.label);

                if let Some(callback) = event.callback {
                    callback(ctx);
                }
            }

            // After each event, drain the guest RTOS scheduler until
            // no runnable tasks remain.
            ctx.drain_rtos_scheduler(self.now)?;
        }

        self.running = false;
        Ok(())
    }

    /// Run until an absolute virtual timestamp.
    pub fn run_until(&mut self, ctx: &mut SimulatorContext, deadline: Tick) -> SimResult<()> {
        self.running = true;

        while self.running {
            let peek_time = self.queue.peek_time();
            match peek_time {
                Some(at) if at <= deadline => {
                    // There's an event before or at deadline; dispatch it.
                    let event = self.queue.pop_next().unwrap();
                    if let Some(ref key) = event.key {
                        if key.at < self.now {
                            return Err(SimError::TimeWentBackwards {
                                now: self.now,
                                event_at: key.at,
                            });
                        }
                        self.now = key.at;
                        ctx.now = self.now;
                        self.trace.event_dispatch(self.now, key.id, event.label);
                        if let Some(callback) = event.callback {
                            callback(ctx);
                        }
                    }
                    ctx.drain_rtos_scheduler(self.now)?;
                }
                _ => {
                    // No more events before deadline.  Advance to deadline.
                    self.now = deadline;
                    ctx.now = self.now;
                    break;
                }
            }
        }

        self.running = false;
        Ok(())
    }

    /// Run until the event queue is empty.
    pub fn run_until_idle(&mut self, ctx: &mut SimulatorContext) -> SimResult<()> {
        self.run(ctx)
    }

    /// Stop the run loop at the next iteration.
    pub fn stop(&mut self) {
        self.running = false;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_loop_basic() {
        let config = SimConfig::default();
        let mut core = SimulatorCore::new(config);

        let trace_ptr: *mut TraceSink = &mut core.trace;
        let mut ctx = SimulatorContext::new(trace_ptr);

        core.schedule_at(100, 10, "early", Box::new(|_| {}));
        core.schedule_at(50, 10, "late", Box::new(|_| {}));

        core.run(&mut ctx).unwrap();

        assert_eq!(core.now, 100);
        assert!(core.trace.len() >= 4); // 2 schedule + 2 dispatch
        assert!(core.queue.is_empty());
    }

    #[test]
    fn test_time_rollback_detection() {
        let config = SimConfig::default();
        let mut core = SimulatorCore::new(config);
        core.now = 500; // Artificially advance time

        let trace_ptr: *mut TraceSink = &mut core.trace;
        let mut ctx = SimulatorContext::new(trace_ptr);

        core.schedule_at(100, 10, "past", Box::new(|_| {}));

        let result = core.run(&mut ctx);
        assert!(result.is_err());
        match result.unwrap_err() {
            SimError::TimeWentBackwards { now, event_at } => {
                assert_eq!(now, 500);
                assert_eq!(event_at, 100);
            }
            _ => panic!("expected TimeWentBackwards"),
        }
    }

    #[test]
    fn test_run_until_deadline() {
        let config = SimConfig::default();
        let mut core = SimulatorCore::new(config);

        let trace_ptr: *mut TraceSink = &mut core.trace;
        let mut ctx = SimulatorContext::new(trace_ptr);

        core.schedule_at(100, 10, "a", Box::new(|_| {}));
        core.schedule_at(200, 10, "b", Box::new(|_| {}));
        core.schedule_at(300, 10, "c", Box::new(|_| {}));

        core.run_until(&mut ctx, 150).unwrap();

        assert_eq!(core.now, 150);
        assert!(!core.queue.is_empty());

        assert_eq!(core.queue.peek_time(), Some(200));
    }

    #[test]
    fn test_cancel_event() {
        let config = SimConfig::default();
        let mut core = SimulatorCore::new(config);

        let trace_ptr: *mut TraceSink = &mut core.trace;
        let mut ctx = SimulatorContext::new(trace_ptr);

        let id = core.schedule_at(100, 10, "will_cancel", Box::new(|_| {}));
        core.schedule_at(200, 10, "should_fire", Box::new(|_| {}));
        core.cancel(id);

        core.run(&mut ctx).unwrap();

        assert_eq!(core.now, 200);
        assert!(core.queue.is_empty());
    }
}
