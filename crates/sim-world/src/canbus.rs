//! Deterministic broadcast CAN bus for multi-machine simulation.
//!
//! A [`CanBus`] connects multiple machines.  When a machine sends a CAN
//! frame, it is delivered to **all** other attached machines after a
//! configurable latency.  Frames are ordered deterministically by
//! (virtual time, message ID priority, sequence number).
//!
//! # Fault injection
//!
//! - `drop_frame(id)` — silently drops all future frames with the given ID.
//! - `delay_frame(id, extra_ticks)` — adds extra delivery latency to frames
//!   with the given ID.
//! - `corrupt_byte(offset, mask)` — XORs bytes at the given offset in every
//!   subsequent frame.
//!
//! # Example
//!
//! ```rust
//! use sim_world::CanBus;
//!
//! let mut bus = CanBus::new("vcan0", 500);
//! bus.attach(0); bus.attach(1); bus.attach(2);
//!
//! bus.send(0, 0x001, &[1, 2, 3], 0);
//! assert_eq!(bus.next_arrival_time(), Some(500));
//!
//! let rx = bus.drain_arrived(500);
//! // Machine 1 and 2 each get a copy.
//! assert_eq!(rx.len(), 2);
//! ```

use sim_core::Tick;

/// A pending CAN frame waiting for delivery.
#[derive(Debug, Clone)]
struct PendingFrame {
    /// Virtual time when the frame arrives.
    arrival: Tick,
    /// Machine ID of the receiver.
    receiver: u64,
    /// Machine ID of the sender (for trace events).
    sender: u64,
    /// CAN frame identifier.
    id: u32,
    /// Frame payload.
    data: Vec<u8>,
    /// Monotonic sequence number for deterministic tie-breaking.
    seq: u64,
    /// Forwarding hop count. 0 = original send; 1 = forwarded once (e.g. by a
    /// gateway bridge). Used for loop prevention (a forwarded frame is never
    /// forwarded again).
    hop: u8,
    /// For a forwarded frame, the correlation id of the frame that caused the
    /// forward (parent causality); 0 for an original send.
    parent_correlation: u64,
}

/// Deterministic broadcast CAN bus.
///
/// Frames are delivered to all attached nodes except the sender after
/// a configurable latency.  Ordering is deterministic: frames are sorted
/// by (arrival time, message ID priority, sequence number).
#[derive(Debug, Clone)]
pub struct CanBus {
    /// Human-readable bus name.
    pub name: String,

    /// Delivery latency in microseconds (virtual-time ticks at 1 µs/tick).
    pub latency_us: u64,

    /// Machine IDs attached to this bus.
    nodes: Vec<u64>,

    /// Pending frame deliveries, sorted by (arrival, id, seq).
    pending: Vec<PendingFrame>,

    /// Next monotonic sequence number.
    next_seq: u64,

    // ── Fault injection state ───────────────────────────────────
    /// Frame IDs to silently drop.
    dropped_ids: Vec<u32>,

    /// Frame IDs whose delivery should be delayed by extra ticks.
    delayed_ids: Vec<(u32, Tick)>,

    /// Byte-level corruption: (offset, mask).  Every frame's byte at
    /// `offset` is XORed with `mask`.
    corrupt: Vec<(usize, u8)>,
}

impl CanBus {
    /// Create a new CAN bus with the given name and per-message latency
    /// in microseconds (virtual-time ticks).
    ///
    /// No machines are attached yet.  Use [`attach`](Self::attach) to add
    /// nodes.
    pub fn new(name: &str, latency_us: u64) -> Self {
        Self {
            name: name.to_string(),
            latency_us,
            nodes: Vec::new(),
            pending: Vec::new(),
            next_seq: 0,
            dropped_ids: Vec::new(),
            delayed_ids: Vec::new(),
            corrupt: Vec::new(),
        }
    }

    /// Attach a machine (by its machine ID) to this bus.
    ///
    /// A machine can only be attached once.  Duplicate attaches are
    /// silently ignored.
    pub fn attach(&mut self, machine_id: u64) {
        if !self.nodes.contains(&machine_id) {
            self.nodes.push(machine_id);
        }
    }

    /// Return the machine IDs attached to this bus.
    pub fn nodes(&self) -> &[u64] {
        &self.nodes
    }

    /// Number of nodes attached to this bus.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    // ── Fault injection ─────────────────────────────────────────

    /// Silently drop all future frames with the given CAN ID.
    pub fn drop_frame(&mut self, id: u32) {
        if !self.dropped_ids.contains(&id) {
            self.dropped_ids.push(id);
        }
    }

    /// Add extra delivery latency (in virtual-time ticks) to all future
    /// frames with the given CAN ID.
    pub fn delay_frame(&mut self, id: u32, extra_ticks: Tick) {
        // Update existing entry or add new one.
        for (existing_id, saved_extra) in &mut self.delayed_ids {
            if *existing_id == id {
                *saved_extra = extra_ticks;
                return;
            }
        }
        self.delayed_ids.push((id, extra_ticks));
    }

    /// Apply a byte-corruption mask at the given offset in every
    /// subsequent frame.
    ///
    /// The mask is XORed with the byte at `offset` (if the frame is
    /// long enough).  Multiple corrupt rules can be active simultaneously.
    pub fn corrupt_byte(&mut self, offset: usize, mask: u8) {
        self.corrupt.push((offset, mask));
    }

    /// Clear all fault injection state.
    pub fn clear_faults(&mut self) {
        self.dropped_ids.clear();
        self.delayed_ids.clear();
        self.corrupt.clear();
    }

    // ── Frame delivery ───────────────────────────────────────────

    /// Send a CAN frame onto the bus.
    ///
    /// The frame is delivered to **all** attached nodes except `sender`
    /// after `latency_us` virtual-time ticks.  If `sender` is not attached,
    /// the frame is still delivered to all attached nodes.
    ///
    /// Returns the number of deliveries queued (one per receiver).
    pub fn send(&mut self, sender: u64, id: u32, data: &[u8], at: Tick) -> usize {
        self.enqueue(sender, id, data, at, 0, 0)
    }

    /// Forward an already-received frame onto this bus (e.g. a gateway bridging
    /// one bus to another). Like [`send`](Self::send) but marks the queued
    /// frames as forwarded (`hop = 1`) so they are never forwarded again, and
    /// carries the `parent_correlation` of the frame that caused the forward for
    /// parent/child causality in trace v2.
    pub fn forward(
        &mut self,
        sender: u64,
        id: u32,
        data: &[u8],
        at: Tick,
        parent_correlation: u64,
    ) -> usize {
        self.enqueue(sender, id, data, at, 1, parent_correlation)
    }

    /// Internal queue routine shared by [`send`](Self::send) and
    /// [`forward`](Self::forward).
    fn enqueue(
        &mut self,
        sender: u64,
        id: u32,
        data: &[u8],
        at: Tick,
        hop: u8,
        parent_correlation: u64,
    ) -> usize {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);

        // Check fault injection: drop?
        if self.dropped_ids.contains(&id) {
            return 0;
        }

        // Check fault injection: extra delay?
        let extra = self
            .delayed_ids
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, t)| *t)
            .unwrap_or(0);

        let base_arrival = at.saturating_add(self.latency_us);
        let arrival = base_arrival.saturating_add(extra);

        // Apply corruption.
        let data = if self.corrupt.is_empty() {
            data.to_vec()
        } else {
            let mut d = data.to_vec();
            for &(offset, mask) in &self.corrupt {
                if offset < d.len() {
                    d[offset] ^= mask;
                }
            }
            d
        };

        // Queue delivery to each non-sender node.
        let mut count = 0;
        for &node_id in &self.nodes {
            if node_id != sender {
                self.pending.push(PendingFrame {
                    arrival,
                    receiver: node_id,
                    sender,
                    id,
                    data: data.clone(),
                    seq,
                    hop,
                    parent_correlation,
                });
                count += 1;
            }
        }

        // Sort by (arrival, id, seq) for deterministic ordering.
        self.pending.sort_by(|a, b| {
            a.arrival
                .cmp(&b.arrival)
                .then_with(|| a.id.cmp(&b.id))
                .then_with(|| a.seq.cmp(&b.seq))
        });

        count
    }

    /// Return the earliest pending frame arrival time, or `None` if empty.
    pub fn next_arrival_time(&self) -> Option<Tick> {
        self.pending.first().map(|p| p.arrival)
    }

    /// Drain all frames whose arrival time is ≤ `now`.
    ///
    /// Returns a vector of `(receiver, sender, frame_id, payload, seq, hop,
    /// parent_correlation)` tuples in deterministic order.  All frames from a
    /// single [`send`](Self::send) call share the same `seq`, so callers can use
    /// it to correlate a transmit with its receive edges.  `hop` is 1 for a
    /// forwarded frame (see [`forward`](Self::forward)), 0 otherwise.
    #[allow(clippy::type_complexity)]
    pub fn drain_arrived(&mut self, now: Tick) -> Vec<(u64, u64, u32, Vec<u8>, u64, u8, u64)> {
        let split_idx = self.pending.partition_point(|p| p.arrival <= now);
        self.pending
            .drain(..split_idx)
            .map(|p| {
                (
                    p.receiver,
                    p.sender,
                    p.id,
                    p.data,
                    p.seq,
                    p.hop,
                    p.parent_correlation,
                )
            })
            .collect()
    }

    /// Number of frames still pending delivery.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canbus_creation() {
        let bus = CanBus::new("vcan0", 500);
        assert_eq!(bus.name, "vcan0");
        assert_eq!(bus.latency_us, 500);
        assert_eq!(bus.node_count(), 0);
        assert_eq!(bus.next_arrival_time(), None);
        assert_eq!(bus.pending_count(), 0);
    }

    #[test]
    fn test_attach_nodes() {
        let mut bus = CanBus::new("vcan0", 500);
        bus.attach(1);
        bus.attach(2);
        bus.attach(3);
        assert_eq!(bus.node_count(), 3);

        // Duplicate attach is ignored.
        bus.attach(1);
        assert_eq!(bus.node_count(), 3);
    }

    #[test]
    fn test_basic_send_deliver() {
        let mut bus = CanBus::new("vcan0", 500);
        bus.attach(1);
        bus.attach(2);
        bus.attach(3);

        // Machine 1 sends frame 0x001 at time 0.
        let n = bus.send(1, 0x001, &[0xAA, 0xBB], 0);
        // Delivered to machines 2 and 3 (not sender).
        assert_eq!(n, 2);
        assert_eq!(bus.pending_count(), 2);
        assert_eq!(bus.next_arrival_time(), Some(500));

        // Nothing arrives before 500.
        let rx = bus.drain_arrived(499);
        assert!(rx.is_empty());
        assert_eq!(bus.pending_count(), 2);

        // Frames arrive at 500.
        let rx = bus.drain_arrived(500);
        assert_eq!(rx.len(), 2);
        // Both deliveries have the correct frame id and data.
        for (_receiver, _sender, id, data, _seq, _hop, _parent) in &rx {
            assert_eq!(*id, 0x001);
            assert_eq!(data, &[0xAA, 0xBB]);
        }
        assert_eq!(bus.pending_count(), 0);
        assert_eq!(bus.next_arrival_time(), None);
    }

    #[test]
    fn test_sender_excluded() {
        let mut bus = CanBus::new("vcan0", 100);
        bus.attach(1);
        bus.attach(2);

        // Machine 1 sends — only machine 2 receives.
        bus.send(1, 0x001, &[1], 0);
        assert_eq!(bus.pending_count(), 1);

        // Machine 2 sends — only machine 1 receives.
        bus.send(2, 0x002, &[2], 0);
        assert_eq!(bus.pending_count(), 2);

        let rx = bus.drain_arrived(100);
        assert_eq!(rx.len(), 2);
    }

    #[test]
    fn test_deterministic_ordering() {
        let mut bus = CanBus::new("vcan0", 500);
        bus.attach(1);
        bus.attach(2);

        // Send frames with different IDs at the same time.
        bus.send(1, 0x100, &[1], 0);
        bus.send(1, 0x010, &[2], 0);
        bus.send(1, 0x001, &[3], 0);

        let rx = bus.drain_arrived(500);
        // Should be ordered by id (lower id first).
        assert_eq!(rx[0].2, 0x001);
        assert_eq!(rx[1].2, 0x010);
        assert_eq!(rx[2].2, 0x100);
    }

    #[test]
    fn test_drop_frame_fault() {
        let mut bus = CanBus::new("vcan0", 500);
        bus.attach(1);
        bus.attach(2);

        bus.drop_frame(0x001);

        let n = bus.send(1, 0x001, &[1], 0);
        assert_eq!(n, 0); // Dropped
        let n = bus.send(1, 0x002, &[2], 0);
        assert_eq!(n, 1); // Not dropped
        assert_eq!(bus.pending_count(), 1);
    }

    #[test]
    fn test_delay_frame_fault() {
        let mut bus = CanBus::new("vcan0", 500);
        bus.attach(1);
        bus.attach(2);

        bus.delay_frame(0x001, 1000);

        // Normal frame arrives at 500.
        bus.send(1, 0x002, &[2], 0);

        // Delayed frame arrives at 500 + 1000 = 1500.
        bus.send(1, 0x001, &[1], 0);

        assert_eq!(bus.next_arrival_time(), Some(500));

        let rx = bus.drain_arrived(500);
        assert_eq!(rx.len(), 1);
        assert_eq!(rx[0].2, 0x002);

        assert_eq!(bus.next_arrival_time(), Some(1500));

        let rx = bus.drain_arrived(1500);
        assert_eq!(rx.len(), 1);
        assert_eq!(rx[0].2, 0x001);
    }

    #[test]
    fn test_corrupt_byte_fault() {
        let mut bus = CanBus::new("vcan0", 500);
        bus.attach(1);
        bus.attach(2);

        bus.corrupt_byte(0, 0xFF);

        bus.send(1, 0x001, &[0xAA, 0xBB], 0);

        let rx = bus.drain_arrived(500);
        assert_eq!(rx.len(), 1);
        // Byte 0 is 0xAA ^ 0xFF = 0x55.
        assert_eq!(rx[0].3[0], 0x55);
        assert_eq!(rx[0].3[1], 0xBB); // Unchanged
    }

    #[test]
    fn test_clear_faults() {
        let mut bus = CanBus::new("vcan0", 500);
        bus.attach(1);
        bus.attach(2);

        bus.drop_frame(0x001);
        bus.delay_frame(0x002, 100);
        bus.corrupt_byte(0, 0xFF);

        bus.clear_faults();

        // Frame should now be delivered normally.
        let n = bus.send(1, 0x001, &[1], 0);
        assert_eq!(n, 1);

        let rx = bus.drain_arrived(500);
        assert_eq!(rx.len(), 1);
        assert_eq!(rx[0].3[0], 1); // Not corrupted
    }

    #[test]
    fn test_send_from_unattached_node() {
        let mut bus = CanBus::new("vcan0", 500);
        bus.attach(1);
        bus.attach(2);

        // Machine 99 is not attached — frame still goes to all attached nodes.
        let n = bus.send(99, 0x001, &[1], 0);
        assert_eq!(n, 2); // Delivered to both 1 and 2
        assert_eq!(bus.pending_count(), 2);
    }
}
