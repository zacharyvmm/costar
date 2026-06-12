//! Simulator configuration.

/// Global simulator configuration. Cheap to copy, immutable for the
/// lifetime of a run.
#[derive(Debug, Clone, Copy)]
pub struct SimConfig {
    /// Virtual tick period in nanoseconds.
    pub tick_period_ns: u64,
    /// Whether the host poller may be used. Deterministic tests must set
    /// this to `false`.
    pub host_io_enabled: bool,
    /// Wall-clock watchdog, in seconds. `None` disables the watchdog.
    pub wall_clock_watchdog_secs: Option<u64>,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            tick_period_ns: 1,
            host_io_enabled: false,
            wall_clock_watchdog_secs: Some(30),
        }
    }
}
