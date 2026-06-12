//! Host-connected mode: non-blocking socket integration via `polling`.
//!
//! When enabled, simulated tasks can interact with real host sockets.
//! A task that would block on `recv()` / `accept()` instead registers
//! interest with the poller, yields, and is woken when data arrives.
//!
//! # Determinism warning
//!
//! Host-connected mode is **not** deterministic. Use deterministic
//! packet scripts (`SimNetDevice::inject_rx`) for golden-trace tests.
//!
//! # MVP status
//!
//! This module is a placeholder. Full host poller integration is planned
//! for a later milestone. The deterministic path is sufficient for the MVP.

/// Placeholder for the host poller adapter.
///
/// Future API sketch:
///
/// ```rust,ignore
/// pub struct HostPoller {
///     poller: polling::Poller,
///     events: polling::Events,
///     sockets: BTreeMap<usize, HostSocket>,
/// }
///
/// impl HostPoller {
///     pub fn new() -> Self;
///     pub fn register_tcp_listener(&mut self, addr: SocketAddr) -> io::Result<usize>;
///     pub fn poll(&mut self, timeout: Option<Duration>) -> io::Result<Vec<HostEvent>>;
/// }
/// ```
pub struct HostPoller;

impl HostPoller {
    /// Create a new host poller (no-op in MVP).
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self
    }
}

impl Default for HostPoller {
    fn default() -> Self {
        Self::new()
    }
}
