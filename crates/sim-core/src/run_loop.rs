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
    ///
    /// Dispatches every live event whose timestamp is `<= deadline`, in
    /// order, then advances virtual time to exactly `deadline`.  On exit
    /// `self.now == deadline` (time never overshoots the deadline and never
    /// moves backwards).
    ///
    /// # Correctness
    ///
    /// This uses [`EventQueue::peek_live_time`], which drains leading
    /// tombstones so the peeked timestamp always belongs to the event the
    /// following `pop_next()` returns.  That closes two historical bugs:
    ///
    /// 1. A cancelled event at the top of the heap could make the old
    ///    `peek_time()` report a tick `<= deadline`, after which
    ///    `pop_next().unwrap()` panicked because the only remaining entries
    ///    were tombstones (it returned `None`).
    /// 2. When a cancelled early event masked a later live event, the old
    ///    code dispatched that live event even though its timestamp was
    ///    *beyond* the deadline, overshooting virtual time.
    pub fn run_until(&mut self, ctx: &mut SimulatorContext, deadline: Tick) -> SimResult<()> {
        self.running = true;

        while self.running {
            match self.queue.peek_live_time() {
                Some(at) if at <= deadline => {
                    // `peek_live_time` guarantees the next `pop_next` returns
                    // this live event at `at`; the `else` arm is purely
                    // defensive and should be unreachable.
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
                    ctx.drain_rtos_scheduler(self.now)?;
                }
                // No live event at or before the deadline (queue empty, or
                // the next live event is beyond the deadline): stop here.
                _ => break,
            }
        }

        // `run_until` advances virtual time to the deadline even when no
        // event sits exactly on it.  Never move backwards: an event may have
        // already advanced `now`, but it can only be `<= deadline` because we
        // never dispatch events beyond the deadline.
        if self.now < deadline {
            self.now = deadline;
            ctx.now = self.now;
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

    // ── run_until regression tests (stabilization plan) ─────────────────

    /// Regression: a cancelled event whose tombstone sits at the top of the
    /// heap must not make `run_until` panic via `pop_next().unwrap()`.
    #[test]
    fn test_run_until_cancelled_tombstone_no_panic() {
        let config = SimConfig::default();
        let mut core = SimulatorCore::new(config);

        let trace_ptr: *mut TraceSink = &mut core.trace;
        let mut ctx = SimulatorContext::new(trace_ptr);

        let id = core.schedule_at(100, 10, "will_cancel", Box::new(|_| {}));
        core.cancel(id);

        // Deadline past the cancelled event: previously panicked.
        core.run_until(&mut ctx, 500).unwrap();

        assert_eq!(core.now, 500);
        assert!(core.queue.is_empty());
    }

    /// Regression: when a cancelled early event masks a later live event,
    /// `run_until` must NOT dispatch the live event beyond the deadline, and
    /// must not overshoot virtual time.
    #[test]
    fn test_run_until_no_overshoot_past_cancelled() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let config = SimConfig::default();
        let mut core = SimulatorCore::new(config);

        let trace_ptr: *mut TraceSink = &mut core.trace;
        let mut ctx = SimulatorContext::new(trace_ptr);

        let fired: Rc<RefCell<Vec<u64>>> = Rc::new(RefCell::new(Vec::new()));

        // Cancelled event at t=100 (earlier than the live one).
        let id = core.schedule_at(100, 10, "cancelled", Box::new(|_| {}));
        // Live event beyond the deadline.
        let f = fired.clone();
        core.schedule_at(
            500,
            10,
            "beyond",
            Box::new(move |_| f.borrow_mut().push(500)),
        );
        core.cancel(id);

        core.run_until(&mut ctx, 200).unwrap();

        // The t=500 event must not have fired, time must stop at the deadline,
        // and the live event must remain queued for a later segment.
        assert!(
            fired.borrow().is_empty(),
            "event beyond deadline fired early"
        );
        assert_eq!(core.now, 200);
        assert_eq!(core.queue.peek_time(), Some(500));
        assert!(!core.queue.is_empty());
    }

    /// Regression: every remaining event cancelled -> no panic, advance to
    /// deadline, and the queue reports empty.
    #[test]
    fn test_run_until_all_cancelled() {
        let config = SimConfig::default();
        let mut core = SimulatorCore::new(config);

        let trace_ptr: *mut TraceSink = &mut core.trace;
        let mut ctx = SimulatorContext::new(trace_ptr);

        let a = core.schedule_at(100, 10, "a", Box::new(|_| {}));
        let b = core.schedule_at(150, 10, "b", Box::new(|_| {}));
        let c = core.schedule_at(180, 10, "c", Box::new(|_| {}));
        core.cancel(a);
        core.cancel(b);
        core.cancel(c);

        core.run_until(&mut ctx, 300).unwrap();

        assert_eq!(core.now, 300);
        assert!(core.queue.is_empty());
    }

    /// `run_until` dispatches events up to and including the deadline tick.
    #[test]
    fn test_run_until_inclusive_of_deadline() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let config = SimConfig::default();
        let mut core = SimulatorCore::new(config);

        let trace_ptr: *mut TraceSink = &mut core.trace;
        let mut ctx = SimulatorContext::new(trace_ptr);

        let fired: Rc<RefCell<Vec<u64>>> = Rc::new(RefCell::new(Vec::new()));
        for at in [100u64, 150, 200] {
            let f = fired.clone();
            core.schedule_at(at, 10, "e", Box::new(move |_| f.borrow_mut().push(at)));
        }

        core.run_until(&mut ctx, 150).unwrap();

        assert_eq!(*fired.borrow(), vec![100, 150]);
        assert_eq!(core.now, 150);
        assert_eq!(core.queue.peek_time(), Some(200));
    }

    /// Deterministic-replay confidence: stepping to a deadline in many small
    /// `run_until` segments dispatches events in exactly the same order and
    /// leaves virtual time in the same place as one continuous `run`.
    #[test]
    fn test_stepped_equals_continuous() {
        use std::cell::RefCell;
        use std::rc::Rc;

        fn schedule_all(core: &mut SimulatorCore, fired: Rc<RefCell<Vec<u64>>>) {
            // Mixed timestamps + priorities, plus one cancellation.
            let ats = [10u64, 10, 25, 40, 40, 55, 70, 90];
            let prios = [20u16, 10, 15, 30, 10, 5, 12, 8];
            let mut cancel_id = None;
            for (i, (&at, &prio)) in ats.iter().zip(prios.iter()).enumerate() {
                let f = fired.clone();
                let id =
                    core.schedule_at(at, prio, "e", Box::new(move |_| f.borrow_mut().push(at)));
                if i == 3 {
                    cancel_id = Some(id);
                }
            }
            if let Some(id) = cancel_id {
                core.cancel(id);
            }
        }

        // Continuous run.
        let cont_fired: Rc<RefCell<Vec<u64>>> = Rc::new(RefCell::new(Vec::new()));
        let mut cont = SimulatorCore::new(SimConfig::default());
        let cont_trace: *mut TraceSink = &mut cont.trace;
        let mut cont_ctx = SimulatorContext::new(cont_trace);
        schedule_all(&mut cont, cont_fired.clone());
        cont.run_until(&mut cont_ctx, 100).unwrap();

        // Stepped run: advance one tick at a time to the same deadline.
        let step_fired: Rc<RefCell<Vec<u64>>> = Rc::new(RefCell::new(Vec::new()));
        let mut step = SimulatorCore::new(SimConfig::default());
        let step_trace: *mut TraceSink = &mut step.trace;
        let mut step_ctx = SimulatorContext::new(step_trace);
        schedule_all(&mut step, step_fired.clone());
        for deadline in 1..=100u64 {
            step.run_until(&mut step_ctx, deadline).unwrap();
        }

        assert_eq!(*cont_fired.borrow(), *step_fired.borrow());
        assert_eq!(cont.now, step.now);
        assert_eq!(cont.now, 100);
    }
}
