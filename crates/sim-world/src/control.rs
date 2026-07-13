use sim_core::Tick;

use crate::world::World;

/// How long to run the simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunLimit {
    /// Run until the given virtual timestamp.
    Until(Tick),
    /// Run until all machines are idle or stopped.
    ToCompletion,
}

/// Why a [`drive_world`] run stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunTermination {
    /// Reached the requested limit naturally.
    Complete,
    /// A machine or device reported an error.
    Error,
    /// A guest task panicked (caught by catch_unwind).
    Panic,
}

/// Outcome of a [`drive_world`] call.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// Why the run stopped.
    pub termination: RunTermination,
    /// Error message, if any.
    pub error: Option<String>,
}

/// Drive a world forward up to `limit`, catching guest panics.
///
/// This is the main entry-point for both JSON-RPC and gRPC servers.  It
/// funnels every batch through the appropriate [`World`] method and wraps
/// the result in a uniform [`RunOutcome`].
pub fn drive_world(world: &mut World, limit: RunLimit) -> RunOutcome {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match limit {
        RunLimit::Until(deadline) => world.run_until(deadline),
        RunLimit::ToCompletion => world.run(),
    }));

    match result {
        Ok(Ok(())) => RunOutcome {
            termination: RunTermination::Complete,
            error: None,
        },
        Ok(Err(e)) => RunOutcome {
            termination: RunTermination::Error,
            error: Some(e.to_string()),
        },
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
            }
        }
    }
}
