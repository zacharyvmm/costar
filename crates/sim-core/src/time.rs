//! Virtual time for the simulator.

use core::fmt;

/// Monotonic virtual time.
///
/// The unit is configurable. By default the simulator uses nanoseconds
/// (`TICK_PERIOD_NS = 1`). Time is *never* allowed to go backwards inside
/// deterministic mode.
pub type Tick = u64;

/// Default tick period in nanoseconds.
///
/// The MVP uses a 1ns tick so that all common RTOS tick periods
/// (1ms, 10ms, 100µs) are representable as exact integers.
pub const TICK_PERIOD_NS: u64 = 1;

/// A duration in virtual ticks. `Delta(0)` means "no advance".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Delta(pub u64);

impl Delta {
    /// Zero delta.
    pub const ZERO: Delta = Delta(0);

    /// Convert a number of milliseconds into a delta, using the current
    /// tick period (1ns by default).
    pub const fn from_millis(ms: u64) -> Self {
        Delta(ms.saturating_mul(1_000_000))
    }

    /// Convert a number of microseconds into a delta.
    pub const fn from_micros(us: u64) -> Self {
        Delta(us.saturating_mul(1_000))
    }
}

impl From<u64> for Delta {
    fn from(v: u64) -> Self {
        Delta(v)
    }
}

impl fmt::Display for Delta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ticks", self.0)
    }
}
