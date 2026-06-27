//! # sim-net
//!
//! Networking layer: deterministic smoltcp integration and optional host poller.
//!
//! Two modes, as required by HANDOFF.md §10.1:
//! 1. **Deterministic mode** — in-process smoltcp with scripted packet I/O,
//!    no host sockets, all wakeups scheduled by virtual time.
//! 2. **Host-connected mode** — non-blocking sockets via `polling`, task blocks
//!    on I/O instead of busy-waiting.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────┐
//! │  smoltcp stack (TCP/IP, UDP, ICMP)       │
//! ├──────────────────────────────────────────┤
//! │  SimNetDevice  (phy::Device impl)        │
//! │  rx_queue: VecDeque<Vec<u8>>             │
//! │  tx_queue: VecDeque<Vec<u8>>             │
//! ├──────────────────────────────────────────┤
//! │  Trace recording (PacketRx / PacketTx)   │
//! └──────────────────────────────────────────┘
//! ```
//!
//! The device uses separate rx/tx queues (unlike smoltcp's Loopback which
//! uses a single merged queue). This lets us inject packets from test scripts
//! and drain transmitted packets for golden-trace comparison independently.

pub use smoltcp;

use std::cell::RefCell;
use std::collections::BTreeMap;

// ── Re-exports ─────────────────────────────────────────────────────────────

pub mod device;
pub mod eth_device;
// host_poller uses Unix-specific std::os::fd types — not available on Windows.
#[cfg(unix)]
pub mod host_poller;

pub use device::SimNetDevice;
pub use eth_device::VirtualEthDevice;

// ── Thread-local device storage ────────────────────────────────────────────

thread_local! {
    /// All registered network devices, keyed by ID.
    static NET_DEVICES: RefCell<BTreeMap<u32, SimNetDevice>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// Insert or replace a network device.
pub fn net_device_insert(dev: SimNetDevice) {
    NET_DEVICES.with(|m| {
        m.borrow_mut().insert(0, dev);
    });
}

/// Run a closure with mutable access to the default network device (ID 0).
pub fn with_net_device_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut SimNetDevice) -> R,
{
    NET_DEVICES.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&0).map(f)
    })
}

/// Run a closure with immutable access to the default network device (ID 0).
pub fn with_net_device<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&SimNetDevice) -> R,
{
    NET_DEVICES.with(|m| {
        let m = m.borrow();
        m.get(&0).map(f)
    })
}

// ── Ethernet device storage ────────────────────────────────────────────────

thread_local! {
    /// All registered Ethernet devices, keyed by ID.
    static ETH_DEVICES: RefCell<BTreeMap<u32, eth_device::VirtualEthDevice>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// Insert or replace an Ethernet device.
pub fn eth_device_insert(dev: eth_device::VirtualEthDevice) {
    ETH_DEVICES.with(|m| {
        m.borrow_mut().insert(dev.id, dev);
    });
}

/// Run a closure with mutable access to an Ethernet device.
pub fn with_eth_device_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut eth_device::VirtualEthDevice) -> R,
{
    ETH_DEVICES.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to an Ethernet device.
pub fn with_eth_device<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&eth_device::VirtualEthDevice) -> R,
{
    ETH_DEVICES.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::phy::{Device, RxToken, TxToken};
    use smoltcp::time::Instant;

    #[test]
    fn test_sim_net_device_basics() {
        let dev = device::SimNetDevice::new(1500);
        assert_eq!(dev.mtu(), 1500);
        assert!(dev.rx_empty());
        assert!(dev.tx_empty());
    }

    #[test]
    fn test_inject_and_drain() {
        let mut dev = device::SimNetDevice::new(1500);
        let pkt: Vec<u8> = vec![0x00, 0x01, 0x02, 0x03];
        dev.inject_rx(pkt.clone());
        assert!(!dev.rx_empty());

        let tx = dev.drain_tx();
        assert!(tx.is_empty());

        let rx_pkt = dev.drain_rx();
        assert_eq!(rx_pkt.len(), 1);
        assert_eq!(rx_pkt[0], pkt);
    }

    #[test]
    fn test_smoltcp_device_trait() {
        let mut dev = device::SimNetDevice::new(1500);

        // Minimal Ethernet frame
        let mut frame: Vec<u8> = vec![0u8; 64];
        frame[0] = 0xff; // broadcast dest MAC
        frame[1] = 0xff;
        frame[6] = 0x00; // src MAC
        frame[12] = 0x08; // EtherType = IPv4
        frame[13] = 0x00;
        dev.inject_rx(frame.clone());

        // smoltcp receive
        let ts = Instant::from_micros_const(0);
        let result = dev.receive(ts);
        assert!(result.is_some());

        let (rx_token, tx_token) = result.unwrap();

        // Consume both tokens
        rx_token.consume(|data| {
            assert_eq!(data.len(), frame.len());
        });

        // Transmit a frame back via tx_token
        tx_token.consume(14, |buf| {
            buf.copy_from_slice(&frame[..14]);
        });

        // Frame should now be in tx_queue
        assert!(!dev.tx_empty());
        let tx_pkts = dev.drain_tx();
        assert_eq!(tx_pkts.len(), 1);
        assert_eq!(tx_pkts[0].len(), 14);
    }
}
