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
