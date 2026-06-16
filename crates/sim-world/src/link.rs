//! Deterministic FIFO link between two machines.
//!
//! A [`Link`] connects a source machine to a target machine.  Every
//! packet that the source sends is held for a configurable latency
//! (in virtual-time ticks) and then delivered to the target machine.
//!
//! This is the building block for simulated networks — multiple links
//! between multiple machines create a deterministic multi-hop topology.
//!
//! # Example
//!
//! ```rust
//! use sim_world::Link;
//! use sim_core::Tick;
//!
//! let mut link = Link::new(0, 1, 5); // src=0, dst=1, 5-tick latency
//!
//! // Machine 0 sends a packet at virtual time 0.
//! link.send(b"hello", 0);
//! assert_eq!(link.next_arrival_time(), Some(5));
//!
//! // At time 5, the packet arrives at Machine 1.
//! let arrived = link.drain_arrived(5);
//! assert_eq!(arrived.len(), 1);
//! assert_eq!(&arrived[0], b"hello");
//! ```

use sim_core::Tick;

/// Deterministic FIFO link between two machines.
///
/// Every packet sent on the link is held for `latency` virtual-time
/// ticks and then delivered to the target machine.  Delivery is
/// deterministic — same packets, same send times, same arrival times
/// regardless of host OS or execution speed.
#[derive(Debug, Clone)]
pub struct Link {
    /// Source machine ID.
    pub source: u64,

    /// Target machine ID.
    pub target: u64,

    /// Delivery latency in virtual-time ticks.
    pub latency: Tick,

    /// Pending deliveries, sorted by arrival time.
    pending: Vec<(Tick, Vec<u8>)>,
}

impl Link {
    /// Create a new unidirectional link.
    ///
    /// `source` and `target` are machine IDs.  `latency` is the number of
    /// virtual-time ticks between when a packet is sent and when it
    /// arrives at the target.
    pub fn new(source: u64, target: u64, latency: Tick) -> Self {
        Self {
            source,
            target,
            latency,
            pending: Vec::new(),
        }
    }

    /// Send a packet from the source machine at virtual time `send_time`.
    ///
    /// The packet will be available for delivery at `send_time + latency`.
    pub fn send(&mut self, data: &[u8], send_time: Tick) {
        let arrival = send_time.saturating_add(self.latency);
        self.pending.push((arrival, data.to_vec()));
        // Keep sorted by arrival time for deterministic draining.
        self.pending.sort_by_key(|(t, _)| *t);
    }

    /// Return the earliest pending packet arrival time, if any.
    pub fn next_arrival_time(&self) -> Option<Tick> {
        self.pending.first().map(|(t, _)| *t)
    }

    /// Drain all packets whose arrival time is ≤ `now`.
    ///
    /// Returns the packet payloads in arrival-time order.  The caller
    /// is responsible for injecting them into the target machine's
    /// event queue or device model.
    pub fn drain_arrived(&mut self, now: Tick) -> Vec<Vec<u8>> {
        let split_idx = self.pending.partition_point(|(t, _)| *t <= now);
        let arrived: Vec<Vec<u8>> = self.pending.drain(..split_idx).map(|(_, d)| d).collect();
        arrived
    }

    /// Number of packets still in transit.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_link_basic_send_receive() {
        let mut link = Link::new(0, 1, 5);

        // Send at time 0, arrives at time 5.
        link.send(b"packet-0", 0);
        assert_eq!(link.next_arrival_time(), Some(5));

        // Send at time 0, arrives at time 5.
        link.send(b"packet-1", 0);
        assert_eq!(link.next_arrival_time(), Some(5));

        // At time 3, nothing has arrived yet.
        let arrived = link.drain_arrived(3);
        assert!(arrived.is_empty());
        assert_eq!(link.pending_count(), 2);

        // At time 5, both packets arrive.
        let arrived = link.drain_arrived(5);
        assert_eq!(arrived.len(), 2);
        assert_eq!(&arrived[0], b"packet-0");
        assert_eq!(&arrived[1], b"packet-1");
        assert_eq!(link.pending_count(), 0);
        assert_eq!(link.next_arrival_time(), None);
    }

    #[test]
    fn test_link_different_send_times() {
        let mut link = Link::new(0, 1, 10);

        link.send(b"early", 0); // arrives at 10
        link.send(b"late", 5); // arrives at 15

        assert_eq!(link.next_arrival_time(), Some(10));

        let arrived = link.drain_arrived(10);
        assert_eq!(arrived.len(), 1);
        assert_eq!(&arrived[0], b"early");
        assert_eq!(link.next_arrival_time(), Some(15));

        let arrived = link.drain_arrived(15);
        assert_eq!(arrived.len(), 1);
        assert_eq!(&arrived[0], b"late");
        assert_eq!(link.next_arrival_time(), None);
    }

    #[test]
    fn test_link_zero_latency() {
        let mut link = Link::new(0, 1, 0);
        link.send(b"instant", 100);
        assert_eq!(link.next_arrival_time(), Some(100));

        let arrived = link.drain_arrived(100);
        assert_eq!(arrived.len(), 1);
        assert_eq!(&arrived[0], b"instant");
    }

    #[test]
    fn test_link_empty() {
        let link = Link::new(0, 1, 5);
        assert_eq!(link.next_arrival_time(), None);
        assert_eq!(link.pending_count(), 0);

        let arrived = link.clone().drain_arrived(100);
        assert!(arrived.is_empty());
    }
}
