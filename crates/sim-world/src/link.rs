//! Deterministic links between two machines.
//!
//! A [`Link`] connects a source machine to a target machine.  Every
//! data unit that the source sends is held for a configurable latency
//! (in virtual-time ticks) and then delivered to the target machine.
//!
//! Three link types are supported:
//!
//! - **Fifo** — generic packet FIFO with fixed per-packet latency.
//!   Each send delivers the entire payload at once after `latency` ticks.
//!
//! - **Eth** — Ethernet link with fixed per-packet latency.
//!   Structurally identical to Fifo; routes VirtualEthDevice frames
//!   between machines in a World simulation.
//!
//! - **Uart** — per-byte serial link.  Each byte is delivered
//!   individually at the rate implied by the baud rate, respecting
//!   virtual time.  The time per byte is computed from baud rate,
//!   data bits, parity, stop bits, and the simulation tick rate.
//!
//! This is the building block for simulated networks — multiple links
//! between multiple machines create a deterministic multi-hop topology.
//!
//! # Example (Fifo)
//!
//! ```rust
//! use sim_world::Link;
//! use sim_core::Tick;
//!
//! let mut link = Link::new_fifo(0, 1, 5); // src=0, dst=1, 5-tick latency
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
//!
//! # Example (Eth)
//!
//! ```rust
//! use sim_world::Link;
//! use sim_core::Tick;
//!
//! let mut link = Link::new_eth(0, 1, 5); // src=0, dst=1, 5-tick latency
//!
//! // Machine 0 sends an Ethernet frame at virtual time 0.
//! link.send(b"\\x00\\x01\\x02\\x03\\x04\\x05...", 0);
//! assert_eq!(link.next_arrival_time(), Some(5));
//!
//! // At time 5, the frame arrives at Machine 1.
//! let arrived = link.drain_arrived(5);
//! assert_eq!(arrived.len(), 1);
//! ```
//!
//! # Example (Uart)
//!
//! ```rust
//! use sim_world::Link;
//! use sim_core::Tick;
//!
//! // 115200 baud, 8N1, 1 MHz tick rate → ~86 ticks per byte
//! let mut link = Link::new_uart(0, 1, 115200, 8, 'N', 1, 1_000_000);
//!
//! link.send(b"Hi", 0);
//! // 'H' arrives at tick 86, 'i' at 172
//! assert_eq!(link.next_arrival_time(), Some(86));
//!
//! let arrived = link.drain_arrived(86);
//! assert_eq!(arrived.len(), 1);
//! assert_eq!(&arrived[0], b"H");
//! ```

use sim_core::Tick;

/// Deterministic link between two machines.
///
/// Three variants:
/// - [`Link::Fifo`]: whole-packet delivery after a fixed latency.
/// - [`Link::Eth`]: Ethernet link — structurally identical to Fifo for
///   now (whole-packet delivery after a fixed latency).
/// - [`Link::Uart`]: per-byte delivery at the rate implied by the
///   baud rate and serial frame format.
#[derive(Debug, Clone)]
pub enum Link {
    /// Generic packet FIFO with fixed per-packet latency.
    ///
    /// Every packet sent on the link is held for `latency` virtual-time
    /// ticks and then delivered to the target machine.
    Fifo {
        /// Source machine ID.
        source: u64,

        /// Target machine ID.
        target: u64,

        /// Delivery latency in virtual-time ticks.
        latency: Tick,

        /// Pending deliveries, sorted by arrival time.
        pending: Vec<(Tick, Vec<u8>)>,
    },

    /// Ethernet link — whole-packet delivery after a fixed latency.
    ///
    /// Structurally identical to [`Link::Fifo`].  Routes VirtualEthDevice
    /// frames between machines in a World simulation.
    Eth {
        /// Source machine ID.
        source: u64,

        /// Target machine ID.
        target: u64,

        /// Delivery latency in virtual-time ticks.
        latency: Tick,

        /// Pending deliveries, sorted by arrival time.
        pending: Vec<(Tick, Vec<u8>)>,
    },

    /// UART serial link — per-byte delivery at baud rate.
    ///
    /// Each byte is delivered individually.  The time between
    /// consecutive bytes is computed as:
    ///
    /// ```text
    /// bits_per_byte = 1 (start) + data_bits + (parity != 'N') as u64 + stop_bits
    /// ticks_per_byte = (bits_per_byte * tick_rate_hz) / baud
    /// ```
    Uart {
        /// Source machine ID.
        source: u64,

        /// Target machine ID.
        target: u64,

        /// Baud rate (e.g. 115200).
        baud: u32,

        /// Data bits per frame (typically 8, range 5–9).
        data_bits: u8,

        /// Parity: 'N' (none), 'E' (even), 'O' (odd).
        parity: char,

        /// Stop bits (typically 1, range 1–2).
        stop_bits: u8,

        /// Simulation tick rate in Hz (e.g. 1_000_000 for 1 µs ticks).
        tick_rate_hz: u64,

        /// Precomputed virtual ticks per byte.
        ticks_per_byte: u64,

        /// Pending byte deliveries, sorted by arrival time.
        pending: Vec<(Tick, u8)>,
    },
}

impl Link {
    /// Create a new Fifo link.
    ///
    /// `source` and `target` are machine IDs.  `latency` is the number
    /// of virtual-time ticks between when a packet is sent and when it
    /// arrives at the target.
    pub fn new_fifo(source: u64, target: u64, latency: Tick) -> Self {
        Self::Fifo {
            source,
            target,
            latency,
            pending: Vec::new(),
        }
    }

    /// Create a new Ethernet link.
    ///
    /// `source` and `target` are machine IDs.  `latency` is the number
    /// of virtual-time ticks between when a frame is sent and when it
    /// arrives at the target.
    ///
    /// Structurally identical to [`Link::new_fifo`]; the `Eth` variant
    /// exists to let scenarios explicitly declare Ethernet links for
    /// VirtualEthDevice routing.
    pub fn new_eth(source: u64, target: u64, latency: Tick) -> Self {
        Self::Eth {
            source,
            target,
            latency,
            pending: Vec::new(),
        }
    }

    /// Create a new UART link.
    ///
    /// `baud` is the baud rate (e.g. 115200).  `data_bits` is the
    /// number of data bits per frame (typically 8).  `parity` is
    /// 'N' (none), 'E' (even), or 'O' (odd).  `stop_bits` is the
    /// number of stop bits (typically 1).  `tick_rate_hz` is the
    /// simulation tick rate in Hz (e.g. 1_000_000 for 1 µs ticks).
    pub fn new_uart(
        source: u64,
        target: u64,
        baud: u32,
        data_bits: u8,
        parity: char,
        stop_bits: u8,
        tick_rate_hz: u64,
    ) -> Self {
        let bits_per_byte =
            1u64 + u64::from(data_bits) + u64::from(parity != 'N') + u64::from(stop_bits);
        let ticks_per_byte = (bits_per_byte * tick_rate_hz) / u64::from(baud);
        Self::Uart {
            source,
            target,
            baud,
            data_bits,
            parity,
            stop_bits,
            tick_rate_hz,
            ticks_per_byte,
            pending: Vec::new(),
        }
    }

    /// Source machine ID.
    pub fn source(&self) -> u64 {
        match self {
            Link::Fifo { source, .. } | Link::Eth { source, .. } | Link::Uart { source, .. } => {
                *source
            }
        }
    }

    /// Target machine ID.
    pub fn target(&self) -> u64 {
        match self {
            Link::Fifo { target, .. } | Link::Eth { target, .. } | Link::Uart { target, .. } => {
                *target
            }
        }
    }

    /// Whether this is a UART link (vs. Fifo or Eth).
    pub fn is_uart(&self) -> bool {
        matches!(self, Link::Uart { .. })
    }

    /// Send data from the source machine at virtual time `send_time`.
    ///
    /// For [`Link::Fifo`] and [`Link::Eth`], the entire payload is
    /// delivered at `send_time + latency`.
    ///
    /// For [`Link::Uart`], each byte is delivered individually, spaced
    /// by `ticks_per_byte` starting at `send_time + ticks_per_byte`.
    pub fn send(&mut self, data: &[u8], send_time: Tick) {
        match self {
            Link::Fifo {
                latency, pending, ..
            }
            | Link::Eth {
                latency, pending, ..
            } => {
                let arrival = send_time.saturating_add(*latency);
                pending.push((arrival, data.to_vec()));
                pending.sort_by_key(|(t, _)| *t);
            }
            Link::Uart {
                ticks_per_byte,
                pending,
                ..
            } => {
                let mut arrival = send_time.saturating_add(*ticks_per_byte);
                for &byte in data {
                    pending.push((arrival, byte));
                    arrival = arrival.saturating_add(*ticks_per_byte);
                }
                pending.sort_by_key(|(t, _)| *t);
            }
        }
    }

    /// Return the earliest pending data arrival time, if any.
    pub fn next_arrival_time(&self) -> Option<Tick> {
        match self {
            Link::Fifo { pending, .. } | Link::Eth { pending, .. } => {
                pending.first().map(|(t, _)| *t)
            }
            Link::Uart { pending, .. } => pending.first().map(|(t, _)| *t),
        }
    }

    /// Drain all data whose arrival time is ≤ `now`.
    ///
    /// Returns the payloads in arrival-time order.  For [`Link::Fifo`]
    /// and [`Link::Eth`] each inner `Vec<u8>` is a full packet.  For
    /// [`Link::Uart`] each inner `Vec<u8>` is a single byte.
    ///
    /// The caller is responsible for injecting them into the target
    /// machine's event queue or device model.
    pub fn drain_arrived(&mut self, now: Tick) -> Vec<Vec<u8>> {
        match self {
            Link::Fifo { pending, .. } | Link::Eth { pending, .. } => {
                let split_idx = pending.partition_point(|(t, _)| *t <= now);
                let arrived: Vec<Vec<u8>> = pending.drain(..split_idx).map(|(_, d)| d).collect();
                arrived
            }
            Link::Uart { pending, .. } => {
                let split_idx = pending.partition_point(|(t, _)| *t <= now);
                let arrived: Vec<Vec<u8>> =
                    pending.drain(..split_idx).map(|(_, b)| vec![b]).collect();
                arrived
            }
        }
    }

    /// Number of data units still in transit.
    pub fn pending_count(&self) -> usize {
        match self {
            Link::Fifo { pending, .. } | Link::Eth { pending, .. } => pending.len(),
            Link::Uart { pending, .. } => pending.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Fifo tests ──────────────────────────────────────────────────────

    #[test]
    fn test_fifo_basic_send_receive() {
        let mut link = Link::new_fifo(0, 1, 5);

        link.send(b"packet-0", 0);
        assert_eq!(link.next_arrival_time(), Some(5));

        link.send(b"packet-1", 0);
        assert_eq!(link.next_arrival_time(), Some(5));

        let arrived = link.drain_arrived(3);
        assert!(arrived.is_empty());
        assert_eq!(link.pending_count(), 2);

        let arrived = link.drain_arrived(5);
        assert_eq!(arrived.len(), 2);
        assert_eq!(&arrived[0], b"packet-0");
        assert_eq!(&arrived[1], b"packet-1");
        assert_eq!(link.pending_count(), 0);
        assert_eq!(link.next_arrival_time(), None);
    }

    #[test]
    fn test_fifo_different_send_times() {
        let mut link = Link::new_fifo(0, 1, 10);

        link.send(b"early", 0);
        link.send(b"late", 5);

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
    fn test_fifo_zero_latency() {
        let mut link = Link::new_fifo(0, 1, 0);
        link.send(b"instant", 100);
        assert_eq!(link.next_arrival_time(), Some(100));

        let arrived = link.drain_arrived(100);
        assert_eq!(arrived.len(), 1);
        assert_eq!(&arrived[0], b"instant");
    }

    #[test]
    fn test_fifo_empty() {
        let mut link = Link::new_fifo(0, 1, 5);
        assert_eq!(link.next_arrival_time(), None);
        assert_eq!(link.pending_count(), 0);
        assert!(link.drain_arrived(100).is_empty());
    }

    #[test]
    fn test_fifo_source_target_accessors() {
        let link = Link::new_fifo(7, 42, 10);
        assert_eq!(link.source(), 7);
        assert_eq!(link.target(), 42);
        assert!(!link.is_uart());
    }

    // ── Uart tests ──────────────────────────────────────────────────────

    #[test]
    fn test_uart_ticks_per_byte_calculation() {
        // 115200 baud, 8N1, 1 MHz tick rate
        // bits_per_byte = 1 + 8 + 0 + 1 = 10
        // ticks_per_byte = 10 * 1_000_000 / 115200 = 86 (floor)
        let link = Link::new_uart(0, 1, 115200, 8, 'N', 1, 1_000_000);
        if let Link::Uart { ticks_per_byte, .. } = &link {
            assert_eq!(*ticks_per_byte, 86);
        } else {
            panic!("expected Uart variant");
        }
    }

    #[test]
    fn test_uart_ticks_per_byte_with_parity() {
        // 9600 baud, 8E1, 1 MHz tick rate
        // bits_per_byte = 1 + 8 + 1 + 1 = 11
        // ticks_per_byte = 11 * 1_000_000 / 9600 = 1145
        let link = Link::new_uart(0, 1, 9600, 8, 'E', 1, 1_000_000);
        if let Link::Uart { ticks_per_byte, .. } = &link {
            assert_eq!(*ticks_per_byte, 1145);
        } else {
            panic!("expected Uart variant");
        }
    }

    #[test]
    fn test_uart_single_byte_delivery() {
        let mut link = Link::new_uart(0, 1, 115200, 8, 'N', 1, 1_000_000);
        // ticks_per_byte = 86

        link.send(b"H", 0);
        assert_eq!(link.next_arrival_time(), Some(86));

        let arrived = link.drain_arrived(85);
        assert!(arrived.is_empty());

        let arrived = link.drain_arrived(86);
        assert_eq!(arrived.len(), 1);
        assert_eq!(&arrived[0], b"H");
        assert_eq!(link.pending_count(), 0);
    }

    #[test]
    fn test_uart_multi_byte_delivery() {
        let mut link = Link::new_uart(0, 1, 115200, 8, 'N', 1, 1_000_000);
        // ticks_per_byte = 86

        link.send(b"Hi", 0);
        assert_eq!(link.next_arrival_time(), Some(86));
        assert_eq!(link.pending_count(), 2);

        // Drain 'H' at 86.
        let arrived = link.drain_arrived(86);
        assert_eq!(arrived.len(), 1);
        assert_eq!(&arrived[0], b"H");
        assert_eq!(link.pending_count(), 1);
        assert_eq!(link.next_arrival_time(), Some(172));

        // Drain 'i' at 172.
        let arrived = link.drain_arrived(172);
        assert_eq!(arrived.len(), 1);
        assert_eq!(&arrived[0], b"i");
        assert_eq!(link.pending_count(), 0);
    }

    #[test]
    fn test_uart_send_at_offset() {
        let mut link = Link::new_uart(0, 1, 115200, 8, 'N', 1, 1_000_000);
        // ticks_per_byte = 86

        link.send(b"X", 1000);
        assert_eq!(link.next_arrival_time(), Some(1086));

        let arrived = link.drain_arrived(1086);
        assert_eq!(arrived.len(), 1);
        assert_eq!(&arrived[0], b"X");
    }

    #[test]
    fn test_uart_source_target_accessors() {
        let link = Link::new_uart(3, 99, 115200, 8, 'N', 1, 1_000_000);
        assert_eq!(link.source(), 3);
        assert_eq!(link.target(), 99);
        assert!(link.is_uart());
    }

    #[test]
    fn test_uart_empty() {
        let mut link = Link::new_uart(0, 1, 115200, 8, 'N', 1, 1_000_000);
        assert_eq!(link.next_arrival_time(), None);
        assert_eq!(link.pending_count(), 0);
        assert!(link.drain_arrived(1000).is_empty());
    }

    #[test]
    fn test_uart_low_baud_rate() {
        // 300 baud, 7O2, 1 MHz tick rate
        // bits_per_byte = 1 + 7 + 1 + 2 = 11
        // ticks_per_byte = 11 * 1_000_000 / 300 = 36666
        let link = Link::new_uart(0, 1, 300, 7, 'O', 2, 1_000_000);
        if let Link::Uart { ticks_per_byte, .. } = &link {
            assert_eq!(*ticks_per_byte, 36666);
        } else {
            panic!("expected Uart variant");
        }
    }

    // ── Eth tests ──────────────────────────────────────────────────────

    #[test]
    fn test_eth_basic_send_receive() {
        let mut link = Link::new_eth(0, 1, 5);

        link.send(b"frame-0", 0);
        assert_eq!(link.next_arrival_time(), Some(5));

        link.send(b"frame-1", 0);
        assert_eq!(link.next_arrival_time(), Some(5));

        let arrived = link.drain_arrived(3);
        assert!(arrived.is_empty());
        assert_eq!(link.pending_count(), 2);

        let arrived = link.drain_arrived(5);
        assert_eq!(arrived.len(), 2);
        assert_eq!(&arrived[0], b"frame-0");
        assert_eq!(&arrived[1], b"frame-1");
        assert_eq!(link.pending_count(), 0);
        assert_eq!(link.next_arrival_time(), None);
    }

    #[test]
    fn test_eth_different_send_times() {
        let mut link = Link::new_eth(0, 1, 10);

        link.send(b"early", 0);
        link.send(b"late", 5);

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
    fn test_eth_zero_latency() {
        let mut link = Link::new_eth(0, 1, 0);
        link.send(b"instant", 100);
        assert_eq!(link.next_arrival_time(), Some(100));

        let arrived = link.drain_arrived(100);
        assert_eq!(arrived.len(), 1);
        assert_eq!(&arrived[0], b"instant");
    }

    #[test]
    fn test_eth_empty() {
        let mut link = Link::new_eth(0, 1, 5);
        assert_eq!(link.next_arrival_time(), None);
        assert_eq!(link.pending_count(), 0);
        assert!(link.drain_arrived(100).is_empty());
    }

    #[test]
    fn test_eth_source_target_accessors() {
        let link = Link::new_eth(7, 42, 10);
        assert_eq!(link.source(), 7);
        assert_eq!(link.target(), 42);
        assert!(!link.is_uart());
    }
}
