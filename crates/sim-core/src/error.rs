//! Top-level simulator error type.

use thiserror::Error;

use crate::time::Tick;

/// Result alias for simulator operations.
pub type SimResult<T> = Result<T, SimError>;

/// Stable numeric error codes.
///
/// These are intentionally `repr(u32)` and stable across versions so they can
/// be referenced from C and recorded in traces.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SimErrorCode {
    /// `EventQueue` produced an event with `at < now`.
    TimeWentBackwards = 1,
    /// `sim_port_yield` was called outside an active fiber.
    YieldWithoutActiveFiber = 2,
    /// A fiber was resumed twice without a yield in between.
    FiberResumedTwice = 3,
    /// A C task returned control after `sim_task_exit` was called.
    TaskReturnedAfterExit = 4,
    /// A panic crossed the C ABI boundary.
    PanicCrossedCAbi = 5,
    /// The event queue is empty but the run loop was asked to keep going.
    RunLoopDeadlock = 6,
    /// A scheduled callback panicked while it was executing.
    CallbackPanic = 7,
    /// A guest RTOS scheduling drain failed to make progress while guest
    /// tasks still claim to be runnable.
    SchedulerStarvation = 8,
    /// The FreeRTOS port detected a fatal inconsistency.
    PortFatal = 9,
    /// Configuration is invalid.
    InvalidConfig = 10,
}

impl SimErrorCode {
    /// Numeric value of the error code (stable across versions).
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// Top-level simulator error.
#[derive(Debug, Error)]
pub enum SimError {
    /// A virtual-time invariant was violated.
    #[error("simulator time went backwards: now={now}, event_at={event_at}")]
    TimeWentBackwards {
        /// Current virtual time.
        now: Tick,
        /// Time stamped on the event.
        event_at: Tick,
    },
    /// A port-layer operation was attempted without the prerequisite state.
    #[error("simulator port error: {0:?}")]
    Port(SimErrorCode),
    /// A fatal error that the simulator cannot recover from.
    #[error("simulator fatal: {0:?}")]
    Fatal(SimErrorCode),
    /// The event queue ran dry while the run loop expected more events.
    #[error("simulator run loop starved: no events remain and tasks are still runnable")]
    RunLoopDeadlock,
}

impl SimError {
    /// Stable numeric error code, used in traces and across the C ABI.
    pub fn code(&self) -> SimErrorCode {
        match self {
            SimError::TimeWentBackwards { .. } => SimErrorCode::TimeWentBackwards,
            SimError::Port(c) => *c,
            SimError::Fatal(c) => *c,
            SimError::RunLoopDeadlock => SimErrorCode::RunLoopDeadlock,
        }
    }
}
