//! Deterministic virtual network device for smoltcp.
//!
//! Implements `smoltcp::phy::Device` and provides scripted packet
//! injection/drain for deterministic golden-trace tests.
//!
//! # Design
//!
//! Unlike smoltcp's built-in `Loopback` which uses a single merged queue,
//! `SimNetDevice` separates rx and tx queues. This allows:
//! - Injecting packets from test scripts into the rx queue.
//! - Draining transmitted packets from the tx queue for golden-trace comparison.
//! - Independent control of receive and transmit timing.
//!
//! # Example
//!
//! ```rust,no_run
//! use sim_net::SimNetDevice;
//!
//! let mut dev = SimNetDevice::new(1500);
//! dev.inject_rx(vec![0u8; 64]);  // inject a test packet
//!
//! // smoltcp will call receive() and get this packet
//! // after processing, drain_tx() captures output
//! ```

use std::collections::VecDeque;

use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;

/// A deterministic virtual network device backed by in-memory queues.
///
/// Packets are injected into `rx_queue` via [`inject_rx`](Self::inject_rx)
/// and drained from `tx_queue` via [`drain_tx`](Self::drain_tx).
///
/// # smoltcp integration
///
/// `SimNetDevice` implements `smoltcp::phy::Device` so it can be passed
/// directly to `smoltcp::iface::Interface`. The device uses virtual time
/// from the simulator (passed as `Instant`) and never touches host sockets.
#[derive(Debug)]
pub struct SimNetDevice {
    /// Incoming packets (injected by test scripts or host poller).
    rx_queue: VecDeque<Vec<u8>>,
    /// Outgoing packets (captured for golden-trace comparison).
    tx_queue: VecDeque<Vec<u8>>,
    /// Maximum transmission unit.
    mtu: usize,
}

impl SimNetDevice {
    /// Create a new deterministic network device.
    ///
    /// `mtu` is the maximum transmission unit in bytes (typically 1500
    /// for Ethernet, or 65535 for raw IP).
    pub fn new(mtu: usize) -> Self {
        Self {
            rx_queue: VecDeque::new(),
            tx_queue: VecDeque::new(),
            mtu,
        }
    }

    /// Inject a packet into the receive queue.
    ///
    /// The packet will be delivered to smoltcp on the next poll of the
    /// network interface.
    pub fn inject_rx(&mut self, packet: Vec<u8>) {
        self.rx_queue.push_back(packet);
    }

    /// Drain all transmitted packets from the tx queue.
    ///
    /// Returns packets in order of transmission. After this call, the
    /// tx queue is empty.
    pub fn drain_tx(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.tx_queue).into()
    }

    /// Drain all received packets from the rx queue.
    ///
    /// Returns packets in order of injection. After this call, the
    /// rx queue is empty. Useful for checking that all injected
    /// packets were consumed.
    pub fn drain_rx(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.rx_queue).into()
    }

    /// Whether the rx queue is empty.
    pub fn rx_empty(&self) -> bool {
        self.rx_queue.is_empty()
    }

    /// Whether the tx queue is empty.
    pub fn tx_empty(&self) -> bool {
        self.tx_queue.is_empty()
    }

    /// The configured MTU.
    pub fn mtu(&self) -> usize {
        self.mtu
    }
}

impl Device for SimNetDevice {
    type RxToken<'a> = SimRxToken;
    type TxToken<'a> = SimTxToken<'a>;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = self.mtu;
        caps.medium = Medium::Ethernet;
        caps
    }

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.rx_queue.pop_front().map(|buffer| {
            let rx = SimRxToken { buffer };
            let tx = SimTxToken {
                queue: &mut self.tx_queue,
            };
            (rx, tx)
        })
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(SimTxToken {
            queue: &mut self.tx_queue,
        })
    }
}

// ── Rx / Tx tokens ─────────────────────────────────────────────────────────

/// Receive token — wraps a received packet buffer.
#[doc(hidden)]
pub struct SimRxToken {
    buffer: Vec<u8>,
}

impl phy::RxToken for SimRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.buffer)
    }
}

/// Transmit token — writes to the tx queue on consume.
#[doc(hidden)]
#[derive(Debug)]
pub struct SimTxToken<'a> {
    queue: &'a mut VecDeque<Vec<u8>>,
}

impl<'a> phy::TxToken for SimTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = Vec::new();
        buffer.resize(len, 0);
        let result = f(&mut buffer);
        self.queue.push_back(buffer);
        result
    }
}
