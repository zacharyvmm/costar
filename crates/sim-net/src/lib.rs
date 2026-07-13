#![warn(missing_docs)]
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
//!
//! # TCP Bridge (host-connected mode)
//!
//! For interactive mode, [`TcpBridge`] connects a VirtualEthDevice to
//! a remote TCP endpoint, allowing simulated firmware to communicate
//! with real network services using non-blocking I/O.
//!
//! # TAP Bridge (host-connected mode)
//!
//! For interactive mode, [`TapBridge`] creates a host TAP interface and
//! bridges guest Ethernet frames to/from the host network stack.  Raw
//! Ethernet frames are read/written directly on the TAP file descriptor.
//!
//! # Smoltcp Bridge (deterministic mode)
//!
//! [`SmoltcpBridge`] routes guest Ethernet frames through the smoltcp TCP/IP
//! stack, enabling deterministic ARP, ICMP, TCP, and UDP processing.

pub use smoltcp;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;

// ── Re-exports ─────────────────────────────────────────────────────────────

pub mod device;
pub mod eth_device;
// host_poller uses Unix-specific std::os::fd types — not available on Windows.
#[cfg(unix)]
pub mod host_poller;
// TCP bridge for host-connected networking mode (Unix only).
#[cfg(unix)]
pub mod tcp_bridge;
// TAP bridge for host-connected networking mode (Unix only).
#[cfg(unix)]
pub mod tap_bridge;
// Smoltcp bridge for deterministic networking mode.
pub mod bank;
pub mod smoltcp_bridge;

pub use device::SimNetDevice;
pub use eth_device::VirtualEthDevice;
pub use smoltcp_bridge::SmoltcpBridge;
#[cfg(unix)]
pub use tap_bridge::TapBridge;
#[cfg(unix)]
pub use tcp_bridge::TcpBridge;

pub use bank::{
    activate_network_bank, with_network_bank, with_network_bank_if_active, BankGuard, NetworkBank,
};

// ── Thread-local device storage ────────────────────────────────────────────

thread_local! {
    /// All registered network devices, keyed by ID.
    static NET_DEVICES: RefCell<BTreeMap<u32, SimNetDevice>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// Insert or replace a network device.
pub fn net_device_insert(dev: SimNetDevice) {
    if bank::has_active_bank() {
        with_network_bank_if_active(|bank| {
            bank.inner.net_devices.borrow_mut().insert(0, dev);
        });
        return;
    }
    NET_DEVICES.with(|m| {
        m.borrow_mut().insert(0, dev);
    });
}
/// Run a closure with mutable access to the default network device (ID 0).
pub fn with_net_device_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut SimNetDevice) -> R,
{
    let mut f = Some(f);
    if let Some(result) = with_network_bank_if_active(|bank| {
        bank.inner
            .net_devices
            .borrow_mut()
            .get_mut(&0)
            .map(|dev| f.take().unwrap()(dev))
    }) {
        return result;
    }
    NET_DEVICES.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&0).map(|dev| f.take().unwrap()(dev))
    })
}
/// Run a closure with immutable access to the default network device (ID 0).
pub fn with_net_device<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&SimNetDevice) -> R,
{
    let mut f = Some(f);
    if let Some(result) = with_network_bank_if_active(|bank| {
        bank.inner
            .net_devices
            .borrow()
            .get(&0)
            .map(|dev| f.take().unwrap()(dev))
    }) {
        return result;
    }
    NET_DEVICES.with(|m| {
        let m = m.borrow();
        m.get(&0).map(|dev| f.take().unwrap()(dev))
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
    if bank::has_active_bank() {
        with_network_bank_if_active(|bank| {
            bank.inner.eth_devices.borrow_mut().insert(dev.id, dev);
        });
        return;
    }
    ETH_DEVICES.with(|m| {
        m.borrow_mut().insert(dev.id, dev);
    });
}

/// Run a closure with mutable access to an Ethernet device.
pub fn with_eth_device_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut eth_device::VirtualEthDevice) -> R,
{
    let mut f = Some(f);
    if let Some(result) = with_network_bank_if_active(|bank| {
        bank.inner
            .eth_devices
            .borrow_mut()
            .get_mut(&id)
            .map(|dev| f.take().unwrap()(dev))
    }) {
        return result;
    }
    ETH_DEVICES.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(|dev| f.take().unwrap()(dev))
    })
}
/// Run a closure with immutable access to an Ethernet device.
pub fn with_eth_device<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&eth_device::VirtualEthDevice) -> R,
{
    let mut f = Some(f);
    if let Some(result) = with_network_bank_if_active(|bank| {
        bank.inner
            .eth_devices
            .borrow()
            .get(&id)
            .map(|dev| f.take().unwrap()(dev))
    }) {
        return result;
    }
    ETH_DEVICES.with(|m| {
        let m = m.borrow();
        m.get(&id).map(|dev| f.take().unwrap()(dev))
    })
}
// ── Smoltcp bridge storage (deterministic mode) ─────────────────────────────

thread_local! {
    /// The smoltcp bridge (if configured).
    static SMOLTCP_BRIDGE: RefCell<Option<smoltcp_bridge::SmoltcpBridge>> =
        const { RefCell::new(None) };
}

/// Replace the smoltcp bridge with a new one.
pub fn smoltcp_bridge_set(bridge: smoltcp_bridge::SmoltcpBridge) {
    if bank::has_active_bank() {
        with_network_bank_if_active(|bank| {
            *bank.inner.smoltcp_bridge.borrow_mut() = Some(bridge);
        });
        return;
    }
    SMOLTCP_BRIDGE.with(|m| {
        *m.borrow_mut() = Some(bridge);
    });
}

/// Run a closure with mutable access to the smoltcp bridge.
pub fn with_smoltcp_bridge_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut smoltcp_bridge::SmoltcpBridge) -> R,
{
    let mut f = Some(f);
    if let Some(result) = with_network_bank_if_active(|bank| {
        bank.inner
            .smoltcp_bridge
            .borrow_mut()
            .as_mut()
            .map(|b| f.take().unwrap()(b))
    }) {
        return result;
    }
    SMOLTCP_BRIDGE.with(|m| {
        let mut m = m.borrow_mut();
        m.as_mut().map(|b| f.take().unwrap()(b))
    })
}

// ── TCP bridge storage (interactive mode) ───────────────────────────────────

#[cfg(unix)]
thread_local! {
    /// The host TCP bridge (if configured for interactive networking).
    static TCP_BRIDGE: RefCell<Option<tcp_bridge::TcpBridge>> =
        const { RefCell::new(None) };
}

/// Replace the TCP bridge with a new one.
pub fn tcp_bridge_set(bridge: tcp_bridge::TcpBridge) {
    if bank::has_active_bank() {
        with_network_bank_if_active(|bank| {
            *bank.inner.tcp_bridge.borrow_mut() = Some(bridge);
        });
        return;
    }
    TCP_BRIDGE.with(|m| {
        *m.borrow_mut() = Some(bridge);
    });
}

/// Run a closure with mutable access to the TCP bridge.
#[cfg(unix)]
pub fn with_tcp_bridge_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut tcp_bridge::TcpBridge) -> R,
{
    let mut f = Some(f);
    if let Some(result) = with_network_bank_if_active(|bank| {
        bank.inner
            .tcp_bridge
            .borrow_mut()
            .as_mut()
            .map(|b| f.take().unwrap()(b))
    }) {
        return result;
    }
    TCP_BRIDGE.with(|m| {
        let mut m = m.borrow_mut();
        m.as_mut().map(|b| f.take().unwrap()(b))
    })
}

// ── TAP bridge storage (interactive mode) ───────────────────────────────────

#[cfg(unix)]
thread_local! {
    /// The host TAP bridge (if configured for interactive networking).
    static TAP_BRIDGE: RefCell<Option<tap_bridge::TapBridge>> =
        const { RefCell::new(None) };
}

/// Replace the TAP bridge with a new one.
#[cfg(unix)]
pub fn tap_bridge_set(bridge: tap_bridge::TapBridge) {
    if bank::has_active_bank() {
        with_network_bank_if_active(|bank| {
            *bank.inner.tap_bridge.borrow_mut() = Some(bridge);
        });
        return;
    }
    TAP_BRIDGE.with(|m| {
        *m.borrow_mut() = Some(bridge);
    });
}

/// Run a closure with mutable access to the TAP bridge.
#[cfg(unix)]
pub fn with_tap_bridge_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut tap_bridge::TapBridge) -> R,
{
    let mut f = Some(f);
    if let Some(result) = with_network_bank_if_active(|bank| {
        bank.inner
            .tap_bridge
            .borrow_mut()
            .as_mut()
            .map(|b| f.take().unwrap()(b))
    }) {
        return result;
    }
    TAP_BRIDGE.with(|m| {
        let mut m = m.borrow_mut();
        m.as_mut().map(|b| f.take().unwrap()(b))
    })
}

/// Run a closure with immutable access to the TAP bridge.
#[cfg(unix)]
pub fn with_tap_bridge<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&tap_bridge::TapBridge) -> R,
{
    let mut f = Some(f);
    if let Some(result) = with_network_bank_if_active(|bank| {
        bank.inner
            .tap_bridge
            .borrow()
            .as_ref()
            .map(|b| f.take().unwrap()(b))
    }) {
        return result;
    }
    TAP_BRIDGE.with(|m| {
        let m = m.borrow();
        m.as_ref().map(|b| f.take().unwrap()(b))
    })
}
/// so the scheduler wakes up when the host sends frames to the TAP
/// interface.
///
/// # Safety
///
/// The TAP bridge must outlive the host poller registration.
/// The caller must call [`tap_bridge_deregister_from_poller`] before
/// the TAP bridge is dropped.
#[cfg(unix)]
pub fn tap_bridge_register_with_poller() -> io::Result<()> {
    if bank::has_active_bank() {
        return with_network_bank_if_active(|bank| -> io::Result<()> {
            let tap = bank.inner.tap_bridge.borrow();
            if let Some(tap) = tap.as_ref() {
                if tap.is_active() {
                    unsafe {
                        let mut hp = bank.inner.host_poller.borrow_mut();
                        if let Some(hp) = hp.as_mut() {
                            hp.register_raw(tap.raw_fd())?;
                        }
                    }
                }
            }
            Ok(())
        })
        .unwrap_or(Ok(()));
    }
    TAP_BRIDGE.with(|tap_cell| {
        let tap = tap_cell.borrow();
        if let Some(tap) = tap.as_ref() {
            if tap.is_active() {
                unsafe {
                    host_poller::HOST_POLLER.with(|hp_cell| {
                        let mut hp = hp_cell.borrow_mut();
                        if let Some(hp) = hp.as_mut() {
                            hp.register_raw(tap.raw_fd())?;
                        }
                        Ok::<(), io::Error>(())
                    })?;
                }
            }
        }
        Ok(())
    })
}

/// Deregister the TAP bridge's file descriptor from the host poller.
#[cfg(unix)]
pub fn tap_bridge_deregister_from_poller() {
    if bank::has_active_bank() {
        with_network_bank_if_active(|bank| {
            let tap = bank.inner.tap_bridge.borrow();
            if let Some(tap) = tap.as_ref() {
                if tap.is_active() {
                    unsafe {
                        let mut hp = bank.inner.host_poller.borrow_mut();
                        if let Some(hp) = hp.as_mut() {
                            let _ = hp.deregister_raw(tap.raw_fd());
                        }
                    }
                }
            }
        });
        return;
    }
    TAP_BRIDGE.with(|tap_cell| {
        let tap = tap_cell.borrow();
        if let Some(tap) = tap.as_ref() {
            if tap.is_active() {
                unsafe {
                    host_poller::HOST_POLLER.with(|hp_cell| {
                        let mut hp = hp_cell.borrow_mut();
                        if let Some(hp) = hp.as_mut() {
                            let _ = hp.deregister_raw(tap.raw_fd());
                        }
                    });
                }
            }
        }
    });
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
