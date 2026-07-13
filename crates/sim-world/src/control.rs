use sim_core::Tick;

use crate::world::World;

/// How long to run the simulation.
pub enum RunLimit {
    /// Run until the given virtual timestamp.
    Until(Tick),
    /// Run until all machines are idle or stopped.
    ToCompletion,
    /// Run for at most this many [`World::step`] iterations.
    EventCount(u64),
}

/// Why a [`drive_world`] run stopped.
pub enum RunTermination {
    /// Reached the requested limit naturally (deadline, completion, or event count).
    Complete,
    /// The run hit an explicit event-count limit ([`RunLimit::EventCount`]).
    LimitReached,
    /// A machine or device reported an error.
    Error,
    /// A guest task panicked (caught by catch_unwind).
    Panic,
    /// The simulation was paused from outside.
    Paused,
    /// The simulation was stopped from outside.
    Stopped,
}

/// Outcome of a [`drive_world`] call.
pub struct RunOutcome {
    /// Why the run stopped.
    pub termination: RunTermination,
    /// Error message, if any.
    pub error: Option<String>,
    /// Virtual time when the run ended.
    pub now: Tick,
    /// Number of [`World::step`] iterations that advanced time.
    pub events: u64,
}

/// Drive a world forward up to `limit`, catching guest panics.
///
/// This is the main entry-point for both JSON-RPC and gRPC servers.  It
/// loops over [`World::step`], counting events, and wraps the result in a
/// uniform [`RunOutcome`].
pub fn drive_world(world: &mut World, limit: RunLimit) -> RunOutcome {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drive_world_inner(world, limit)
    }));

    match result {
        Ok(outcome) => outcome,
        Err(panic) => {
            let msg = if let Some(s) = panic.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = panic.downcast_ref::<&str>() {
                s.to_string()
            } else {
                "unknown panic".to_string()
            };
            RunOutcome {
                termination: RunTermination::Panic,
                error: Some(msg),
                now: world.now,
                events: 0,
            }
        }
    }
}

/// Inner loop shared by [`drive_world`]; not unwind-safe on its own.
fn drive_world_inner(world: &mut World, limit: RunLimit) -> RunOutcome {
    let max_events = match limit {
        RunLimit::EventCount(n) => Some(n),
        _ => None,
    };
    let deadline = match limit {
        RunLimit::Until(d) => Some(d),
        _ => None,
    };

    let mut events: u64 = 0;

    loop {
        // Honour event-count limit.
        if let Some(max) = max_events {
            if events >= max {
                return RunOutcome {
                    termination: RunTermination::LimitReached,
                    error: None,
                    now: world.now,
                    events,
                };
            }
        }

        // Honour external stop / pause.
        if world.is_paused() {
            return RunOutcome {
                termination: RunTermination::Stopped,
                error: None,
                now: world.now,
                events,
            };
        }

        // Honour deadline: never advance past it.
        if let Some(d) = deadline {
            if world.now >= d {
                return RunOutcome {
                    termination: RunTermination::Complete,
                    error: None,
                    now: world.now,
                    events,
                };
            }
            // Check whether the next event lies past the deadline.
            match world.next_global_event_time() {
                Some(t) if t <= d => { /* proceed */ }
                _ => {
                    return RunOutcome {
                        termination: RunTermination::Complete,
                        error: None,
                        now: world.now,
                        events,
                    };
                }
            }
        }

        match world.step() {
            Ok(crate::world::StepOutcome::Advanced(_)) => {
                events += 1;
            }
            Ok(crate::world::StepOutcome::Done) => {
                return RunOutcome {
                    termination: RunTermination::Complete,
                    error: None,
                    now: world.now,
                    events,
                };
            }
            Err(e) => {
                return RunOutcome {
                    termination: RunTermination::Error,
                    error: Some(e.to_string()),
                    now: world.now,
                    events,
                };
            }
        }
    }
}
