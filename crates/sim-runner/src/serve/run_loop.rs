//! Cooperative JSON-RPC simulation run loop.
//!
//! Long runs execute in bounded tick batches. Between batches the owner checks
//! stop / cancel flags so a sibling TCP connection can stop or inspect the
//! session while another connection's run is active.

use std::sync::atomic::{AtomicBool, Ordering};

use sim_world::{drive_world, RunLimit, RunTermination, SessionState, World};

/// Default virtual ticks advanced per cooperative batch.
pub const DEFAULT_TICK_BATCH: u64 = 1_000;

/// Shared control flags for an in-flight cooperative run.
#[derive(Debug, Default)]
pub struct RunControl {
    stop: AtomicBool,
    cancel: AtomicBool,
}

impl RunControl {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    pub fn cancel_requested(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

/// Outcome of a cooperative drive to completion (or stop/cancel).
pub struct CooperativeOutcome {
    pub state: SessionState,
    pub error: Option<String>,
    #[allow(dead_code)]
    pub events: u64,
}

/// Drive `world` toward completion in `tick_batch`-sized slices, honouring
/// `control` between batches.
///
/// Terminal states:
/// - natural idle / completion → [`SessionState::Done`]
/// - explicit stop → [`SessionState::Done`] (after `world.stop()`)
/// - cancel / disconnect → [`SessionState::Paused`]
/// - error / panic → [`SessionState::Error`]
pub fn drive_cooperative(
    world: &mut World,
    control: &RunControl,
    tick_batch: u64,
    mut on_batch: impl FnMut(&mut World) -> bool,
) -> CooperativeOutcome {
    let tick_batch = tick_batch.max(1);
    let mut events: u64 = 0;

    // Clear a leftover pause so a resumed session can advance.
    if world.is_paused() {
        world.resume();
    }

    loop {
        if control.stop_requested() {
            world.stop();
            return CooperativeOutcome {
                state: SessionState::Done,
                error: None,
                events,
            };
        }
        if control.cancel_requested() {
            return CooperativeOutcome {
                state: SessionState::Paused,
                error: None,
                events,
            };
        }

        if world.all_idle() || world.next_global_event_time().is_none() {
            return CooperativeOutcome {
                state: SessionState::Done,
                error: None,
                events,
            };
        }

        let deadline = world.now.saturating_add(tick_batch);
        let outcome = drive_world(world, RunLimit::Until(deadline));
        events = events.saturating_add(outcome.events);

        match outcome.termination {
            RunTermination::Error | RunTermination::Panic => {
                return CooperativeOutcome {
                    state: SessionState::Error,
                    error: Some(
                        outcome
                            .error
                            .unwrap_or_else(|| "simulation error".to_string()),
                    ),
                    events,
                };
            }
            RunTermination::Stopped => {
                return CooperativeOutcome {
                    state: SessionState::Done,
                    error: None,
                    events,
                };
            }
            RunTermination::Paused => {
                return CooperativeOutcome {
                    state: SessionState::Paused,
                    error: None,
                    events,
                };
            }
            RunTermination::Complete | RunTermination::LimitReached => {}
        }

        // Re-check control immediately after each batch so stop/cancel from a
        // sibling connection is observed without waiting for another slice.
        if control.stop_requested() {
            world.stop();
            return CooperativeOutcome {
                state: SessionState::Done,
                error: None,
                events,
            };
        }
        if control.cancel_requested() {
            return CooperativeOutcome {
                state: SessionState::Paused,
                error: None,
                events,
            };
        }

        // Allow the transport to stream incremental output / detect disconnect.
        if !on_batch(world) {
            control.request_cancel();
            return CooperativeOutcome {
                state: SessionState::Paused,
                error: None,
                events,
            };
        }

        // Yield so sibling TCP connection threads can run stop/status handlers
        // on single-core hosts without waiting for the full simulation.
        std::thread::yield_now();
    }
}
