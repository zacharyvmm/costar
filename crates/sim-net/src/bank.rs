//! Per-World network ownership: [`NetworkBank`] and an RAII active-context guard.
//!
//! # Why this exists
//!
//! Historically every network type stored its instances in per-type thread-local
//! stores. Because two in-process `World`s each use device id `0` for their
//! network devices, those stores collide: one world can observe another world's
//! network state.
//!
//! A [`NetworkBank`] owns one map per network type. Each `World` owns a bank;
//! while its guest firmware executes, the FFI layer activates that bank so the
//! `sim_net::with_*` accessors resolve into *its* network objects. The active
//! stack owns a reference-counted handle to every active bank, so dispatch never
//! relies on a borrowed pointer remaining valid. This mirrors the
//! [`DeviceBank`] pattern in `sim-devices`.
//!
//! # Backward compatibility (byte-identical default)
//!
//! When no bank is active, the accessors fall back to their per-type thread-local
//! stores. Existing single-`World`-per-process code paths therefore behave
//! exactly as before — one thread-local store, single-threaded access — so golden
//! traces stay byte-identical.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::device::SimNetDevice;
use crate::eth_device::VirtualEthDevice;
#[cfg(unix)]
use crate::host_poller::HostPoller;
use crate::smoltcp_bridge::SmoltcpBridge;
#[cfg(unix)]
use crate::tap_bridge::TapBridge;
#[cfg(unix)]
use crate::tcp_bridge::TcpBridge;

/// Owns one instance map per network type for a single `World`.
///
/// Each field is an independent `RefCell` so borrow granularity matches the
/// legacy per-type thread-local maps exactly — accessing one network type never
/// borrows another, so no new re-entrancy/double-borrow hazard is introduced.
#[derive(Clone)]
pub struct NetworkBank {
    /// Shared storage for this handle and any currently active context entry.
    /// `Rc` intentionally keeps a bank thread-affine: network objects are
    /// single-threaded state and an active context is local to one thread.
    pub(crate) inner: Rc<NetworkBankInner>,
}

/// The mutable network storage behind a [`NetworkBank`] handle.
///
/// It is separate from the public handle so an active-context stack can retain
/// ownership without borrowing the caller's handle. The fields remain crate
/// visible because the accessor functions retain their existing fine-grained
/// `RefCell` borrow behavior.
pub(crate) struct NetworkBankInner {
    pub(crate) net_devices: RefCell<BTreeMap<u32, SimNetDevice>>,
    pub(crate) eth_devices: RefCell<BTreeMap<u32, VirtualEthDevice>>,
    pub(crate) smoltcp_bridge: RefCell<Option<SmoltcpBridge>>,
    #[cfg(unix)]
    pub(crate) tcp_bridge: RefCell<Option<TcpBridge>>,
    #[cfg(unix)]
    pub(crate) tap_bridge: RefCell<Option<TapBridge>>,
    #[cfg(unix)]
    pub(crate) host_poller: RefCell<Option<HostPoller>>,
}

impl NetworkBank {
    /// Create an empty network bank.
    pub fn new() -> Self {
        Self {
            inner: Rc::new(NetworkBankInner {
                net_devices: RefCell::new(BTreeMap::new()),
                eth_devices: RefCell::new(BTreeMap::new()),
                smoltcp_bridge: RefCell::new(None),
                #[cfg(unix)]
                tcp_bridge: RefCell::new(None),
                #[cfg(unix)]
                tap_bridge: RefCell::new(None),
                #[cfg(unix)]
                host_poller: RefCell::new(None),
            }),
        }
    }

    /// Run `f` with this bank active on the current thread.
    ///
    /// This is the preferred API for production code. It establishes and
    /// restores the context in one lexical scope, including during panic
    /// unwind, so callers cannot accidentally retain a stale activation.
    pub fn with_active<R>(&self, f: impl FnOnce() -> R) -> R {
        let _guard = self.activate();
        f()
    }

    /// Activate this bank on the current thread.
    ///
    /// Prefer [`with_active`](Self::with_active) for ordinary work. The guard
    /// remains available for compatibility with callers that must compose
    /// several activation guards. Its active stack entry owns a clone of this
    /// bank, so non-LIFO drops and a forgotten guard cannot leave a dangling
    /// active context.
    pub fn activate(&self) -> BankGuard {
        activate_network_bank(self)
    }
}

impl Default for NetworkBank {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    /// Fallback bank used when no [`NetworkBank`] has been activated. Preserves
    /// the legacy single-store-per-thread behavior so existing code is
    /// byte-identical.
    static DEFAULT_BANK: NetworkBank = NetworkBank::new();

    /// Active contexts in activation order. Every entry owns the bank it
    /// selects, which is the lifetime proof for dispatch from
    /// [`with_network_bank`].
    static ACTIVE_BANKS: RefCell<Vec<Rc<ActiveBank>>> = const { RefCell::new(Vec::new()) };
}

struct ActiveBank {
    bank: NetworkBank,
}

/// Resolve the currently active network bank and run `f` against it.
///
/// This is the dispatch point for the per-type accessor functions. When a bank
/// is active it is used; otherwise the thread-local [`DEFAULT_BANK`] is.
#[inline]
pub fn with_network_bank<F, R>(f: F) -> R
where
    F: FnOnce(&NetworkBank) -> R,
{
    // Clone the selected handle before invoking `f`, so no borrow of the
    // thread-local stack is held across arbitrary code. The clone also
    // keeps the selected bank alive for the full callback.
    let active_bank = ACTIVE_BANKS.with(|active| {
        active
            .borrow()
            .last()
            .map(|activation| activation.bank.clone())
    });

    if let Some(bank) = active_bank {
        f(&bank)
    } else {
        DEFAULT_BANK.with(|bank| f(bank))
    }
}

/// Resolve the active network bank (if any) and run `f` against it.
///
/// Returns `Some(result)` if a bank is active, `None` if the caller should
/// fall back to the legacy per-type thread-local store. Unlike
/// [`with_network_bank`], this never falls back to the default bank — it lets
/// the caller maintain backward-compatible golden traces by using the original
/// thread-local stores.
#[inline]
pub fn with_network_bank_if_active<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&NetworkBank) -> R,
{
    ACTIVE_BANKS
        .with(|active| {
            active
                .borrow()
                .last()
                .map(|activation| activation.bank.clone())
        })
        .map(|bank| f(&bank))
}

/// Check whether any [`NetworkBank`] is currently active.
///
/// Used by insert functions to decide whether to route through the bank or
/// the legacy thread-local store before moving a value into a closure.
#[inline]
pub(crate) fn has_active_bank() -> bool {
    ACTIVE_BANKS.with(|a| !a.borrow().is_empty())
}

/// Activate `bank` for the current thread, returning a guard that restores the
/// previous active bank on drop.
///
/// The returned guard owns an activation-stack entry that retains the bank.
/// This is safe even if guards are dropped out of order or intentionally
/// forgotten: the stack never contains a non-owning reference.
pub fn activate_network_bank(bank: &NetworkBank) -> BankGuard {
    let activation = Rc::new(ActiveBank { bank: bank.clone() });
    ACTIVE_BANKS.with(|active| active.borrow_mut().push(activation.clone()));
    BankGuard { activation }
}

/// RAII guard returned by [`activate_network_bank`] /
/// [`NetworkBank::activate`]. On drop it removes its own activation entry,
/// restoring whichever context remains on top of the stack.
#[must_use = "an active network context ends when its guard is dropped"]
pub struct BankGuard {
    activation: Rc<ActiveBank>,
}

impl Drop for BankGuard {
    fn drop(&mut self) {
        // Removing by identity, rather than restoring a cached previous value,
        // keeps nested contexts correct even when guards are dropped out of
        // LIFO order. `try_with` avoids a second panic during TLS teardown.
        let _ = ACTIVE_BANKS.try_with(|active| {
            let mut active = active.borrow_mut();
            if let Some(index) = active
                .iter()
                .rposition(|entry| Rc::ptr_eq(entry, &self.activation))
            {
                active.remove(index);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eth_device::VirtualEthDevice;

    fn make_eth(id: u32, mac: [u8; 6]) -> VirtualEthDevice {
        VirtualEthDevice::new(id, mac, 1500)
    }

    // ── R3: NetworkBank isolation tests ────────────────────────────────

    #[test]
    fn two_banks_eth_device_id_zero_do_not_leak() {
        for seed in 0..100 {
            let bank_a = NetworkBank::new();
            let bank_b = NetworkBank::new();
            let mac_a = [0x02, 0x00, 0x00, 0x00, 0x00, (seed & 0xFF) as u8];
            let mac_b = [0x02, 0x00, 0x00, 0x00, 0x00, ((seed + 1) & 0xFF) as u8];

            // Insert into bank A.
            {
                let _guard = bank_a.activate();
                crate::eth_device_insert(make_eth(0, mac_a));
            }
            // Insert into bank B.
            {
                let _guard = bank_b.activate();
                crate::eth_device_insert(make_eth(0, mac_b));
            }

            // Verify bank A still has its own device.
            {
                let _guard = bank_a.activate();
                let found = crate::with_eth_device_mut(0, |eth| {
                    assert_eq!(eth.mac, mac_a, "seed {seed}: bank A device 0 has wrong MAC");
                });
                assert!(found.is_some(), "seed {seed}: bank A device 0 missing");
            }

            // Verify bank B still has its own device.
            {
                let _guard = bank_b.activate();
                let found = crate::with_eth_device_mut(0, |eth| {
                    assert_eq!(eth.mac, mac_b, "seed {seed}: bank B device 0 wrong");
                });
                assert!(found.is_some(), "seed {seed}: bank B device 0 missing");
            }

            // B-then-A order.
            {
                let _guard = bank_b.activate();
                crate::with_eth_device_mut(0, |eth| assert_eq!(eth.mac, mac_b));
            }
            {
                let _guard = bank_a.activate();
                crate::with_eth_device_mut(0, |eth| assert_eq!(eth.mac, mac_a));
            }
        }
    }

    #[test]
    fn explicit_bank_isolated_from_default_bank() {
        let explicit = NetworkBank::new();
        let mac_explicit = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
        let mac_default = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];

        // Insert into default (no bank active).
        crate::eth_device_insert(make_eth(0, mac_default));

        // Activate explicit bank, insert its own device 0.
        {
            let _guard = explicit.activate();
            crate::eth_device_insert(make_eth(0, mac_explicit));
            crate::with_eth_device_mut(0, |eth| assert_eq!(eth.mac, mac_explicit));
        }

        // After explicit deactivation, default bank device 0 must still
        // have the original MAC.
        crate::with_eth_device_mut(0, |eth| {
            assert_eq!(
                eth.mac, mac_default,
                "default bank device 0 was overwritten by explicit bank"
            );
        });
    }

    #[test]
    fn panic_in_active_network_context_restores_prior_bank() {
        let bank_a = NetworkBank::new();
        let bank_b = NetworkBank::new();
        let mac_a = [0x02, 0xA0, 0x00, 0x00, 0x00, 0x01];

        // Insert into bank A.
        {
            let _guard = bank_a.activate();
            crate::eth_device_insert(make_eth(0, mac_a));
        }

        // Activate bank A, then nested-activate bank B and panic.
        let _guard_a = bank_a.activate();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard_b = bank_b.activate();
            crate::eth_device_insert(make_eth(0, [0x02, 0xB0, 0x00, 0x00, 0x00, 0x02]));
            panic!("simulated panic inside network context B");
        }));
        assert!(result.is_err());

        // After panic unwind, bank A must be restored.
        crate::with_eth_device_mut(0, |eth| {
            assert_eq!(
                eth.mac, mac_a,
                "bank A device 0 was corrupted by panic in bank B"
            );
        });
        drop(_guard_a);
    }

    #[test]
    fn destroy_and_recreate_bank_yields_no_stale_state() {
        let bank1 = NetworkBank::new();
        {
            let _guard = bank1.activate();
            crate::eth_device_insert(make_eth(0, [0x02, 0x00, 0x00, 0x00, 0x00, 0x01]));
        }
        drop(bank1);

        // Create a fresh bank — device 0 must NOT exist.
        let bank2 = NetworkBank::new();
        {
            let _guard = bank2.activate();
            // A fresh bank has no device 0 — assert it before inserting.
            let absent = crate::with_eth_device_mut(0, |_| ());
            assert!(
                absent.is_none(),
                "fresh NetworkBank unexpectedly retained eth device 0"
            );
            let mac_new = [0x02, 0xFF, 0xEE, 0xDD, 0xCC, 0xBB];
            crate::eth_device_insert(make_eth(0, mac_new));
            crate::with_eth_device_mut(0, |eth| assert_eq!(eth.mac, mac_new));
        }
    }

    #[test]
    fn fragmented_net_device_state_isolated_across_banks() {
        let bank_a = NetworkBank::new();
        let bank_b = NetworkBank::new();

        // Insert SimNetDevice into each bank with partial fragments.
        // Fragment A1 goes to bank A, B1 to bank B.
        {
            let _guard = bank_a.activate();
            let mut dev = SimNetDevice::new(1500);
            dev.inject_rx(b"A1".to_vec());
            crate::net_device_insert(dev);
        }
        {
            let _guard = bank_b.activate();
            let mut dev = SimNetDevice::new(1500);
            dev.inject_rx(b"B1".to_vec());
            crate::net_device_insert(dev);
        }

        // Feed completion fragment A2 into bank A; verify B still has B1.
        {
            let _guard = bank_a.activate();
            crate::with_net_device_mut(|dev| {
                dev.inject_rx(b"A2".to_vec());
            });
        }
        {
            let _guard = bank_b.activate();
            crate::with_net_device_mut(|dev| {
                let rx: Vec<Vec<u8>> = dev.drain_rx();
                assert_eq!(
                    rx,
                    vec![b"B1".to_vec()],
                    "bank B should still have only its own fragment B1"
                );
                // Feed completion fragment B2.
                dev.inject_rx(b"B2".to_vec());
            });
        }

        // Feed completion fragment B3; verify A still has A1+A2.
        {
            let _guard = bank_b.activate();
            crate::with_net_device_mut(|dev| {
                dev.inject_rx(b"B3".to_vec());
            });
        }
        {
            let _guard = bank_a.activate();
            crate::with_net_device_mut(|dev| {
                let rx: Vec<Vec<u8>> = dev.drain_rx();
                assert_eq!(
                    rx,
                    vec![b"A1".to_vec(), b"A2".to_vec()],
                    "bank A should have A1+A2, not bank B fragments"
                );
            });
        }

        // Final: drain B's completion fragments (B2+B3) — must not contain A data.
        {
            let _guard = bank_b.activate();
            crate::with_net_device_mut(|dev| {
                let rx: Vec<Vec<u8>> = dev.drain_rx();
                assert_eq!(
                    rx,
                    vec![b"B2".to_vec(), b"B3".to_vec()],
                    "bank B completion data wrong — may have consumed bank A fragments"
                );
            });
        }
    }

    #[test]
    fn smoltcp_bridge_tcp_state_isolated_across_banks() {
        use smoltcp::time::Instant;
        use smoltcp::wire::{EthernetAddress, HardwareAddress};

        let bank_a = NetworkBank::new();
        let bank_b = NetworkBank::new();

        let mac_a = EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x0A]);
        let mac_b = EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x0B]);

        // Insert SmoltcpBridge with distinct MACs into each bank.
        {
            let _guard = bank_a.activate();
            crate::smoltcp_bridge_set(SmoltcpBridge::new(
                Instant::from_millis(0),
                mac_a,
            ));
        }
        {
            let _guard = bank_b.activate();
            crate::smoltcp_bridge_set(SmoltcpBridge::new(
                Instant::from_millis(0),
                mac_b,
            ));
        }

        // Interleave access — each bank must see only its own bridge.
        {
            let _guard = bank_a.activate();
            crate::with_smoltcp_bridge_mut(|b| {
                let hw = b.iface().hardware_addr();
                assert_eq!(
                    hw,
                    HardwareAddress::Ethernet(mac_a),
                    "bank A smoltcp bridge has wrong MAC"
                );
            });
        }
        {
            let _guard = bank_b.activate();
            crate::with_smoltcp_bridge_mut(|b| {
                let hw = b.iface().hardware_addr();
                assert_eq!(
                    hw,
                    HardwareAddress::Ethernet(mac_b),
                    "bank B smoltcp bridge MAC leaked from bank A"
                );
            });
        }

        // B-then-A order.
        {
            let _guard = bank_b.activate();
            crate::with_smoltcp_bridge_mut(|b| {
                assert_eq!(b.iface().hardware_addr(), HardwareAddress::Ethernet(mac_b));
            });
        }
        {
            let _guard = bank_a.activate();
            crate::with_smoltcp_bridge_mut(|b| {
                assert_eq!(b.iface().hardware_addr(), HardwareAddress::Ethernet(mac_a));
            });
        }
    }

    // ── R3: stale-readiness / buffered state after bank destroy+recreate ──

    #[test]
    fn recreate_bank_has_no_stale_smoltcp_bridge() {
        use smoltcp::time::Instant;
        use smoltcp::wire::EthernetAddress;

        let bank1 = NetworkBank::new();
        {
            let _guard = bank1.activate();
            crate::smoltcp_bridge_set(SmoltcpBridge::new(
                Instant::from_millis(0),
                EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x0F]),
            ));
        }
        drop(bank1);

        // Fresh bank must have no smoltcp bridge, no net devices, no eth devices.
        let bank2 = NetworkBank::new();
        {
            let _guard = bank2.activate();
            let smoltcp_absent =
                crate::with_smoltcp_bridge_mut(|_| ()).is_none();
            assert!(
                smoltcp_absent,
                "fresh NetworkBank unexpectedly retained stale SmoltcpBridge"
            );
            let net_absent =
                crate::with_net_device_mut(|_| ()).is_none();
            assert!(
                net_absent,
                "fresh NetworkBank unexpectedly retained stale SimNetDevice"
            );
            let eth_absent =
                crate::with_eth_device_mut(0, |_| ()).is_none();
            assert!(
                eth_absent,
                "fresh NetworkBank unexpectedly retained stale VirtualEthDevice"
            );
        }
    }
}
