//! Per-World device ownership: [`DeviceBank`] and an RAII active-context guard.
//!
//! # Why this exists
//!
//! Historically every virtual-device type stored its instances in a *per-type*
//! thread-local `BTreeMap<u32, T>` keyed only by device id.  Because two
//! in-process `World`s (e.g. two concurrent gRPC sessions, or a fleet run) each
//! use device id `0` for their CAN controller, timer, etc., those maps collide:
//! one world can observe another world's device state.  This is the root cause
//! documented in `microcar/UNBLOCKING.md` §1 (P0a "Per-Machine Execution And
//! Device Ownership").
//!
//! A [`DeviceBank`] owns one map per device type.  Each `World` (later, each
//! `MachineRuntime`) owns a bank; while its guest firmware executes, the FFI
//! layer activates that bank via [`activate_bank`] so the `sim_devices::with_*`
//! accessors resolve into *its* devices.  The active pointer is a
//! **dispatch mechanism only** — the storage lives in the owning bank, never in
//! the thread-local pointer.  This mirrors the proven `with_sim_global` /
//! `activate_sim_global` pattern already used in `sim-ffi` for `SimGlobal`.
//!
//! # Backward compatibility (byte-identical default)
//!
//! When no bank is active, the accessors fall back to a thread-local
//! [`DEFAULT_BANK`].  Existing single-`World`-per-process code paths (which never
//! activate a bank) therefore behave exactly as before — one thread-local store,
//! single-threaded access — so golden traces stay byte-identical.  Per-world
//! isolation is opt-in: it engages only when a caller activates a bank.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::marker::PhantomData;

use crate::{
    FaultInjector, FlatMemoryStore, VirtualAdc, VirtualCan, VirtualDisplay, VirtualEeprom,
    VirtualEntropy, VirtualFlash, VirtualGpio, VirtualHciController, VirtualI2c, VirtualSpi,
    VirtualTempSensor, VirtualTimer, VirtualTouchScreen, VirtualUart,
};

/// Owns one instance map per virtual-device type for a single `World`.
///
/// Each field is an independent `RefCell` so borrow granularity matches the
/// legacy per-type thread-local maps exactly — accessing one device type never
/// borrows another, so no new re-entrancy/double-borrow hazard is introduced.
///
/// The singleton [`FaultInjector`] lives here too so fault injection is also
/// per-world rather than process-global.
pub struct DeviceBank {
    pub(crate) uarts: RefCell<BTreeMap<u32, VirtualUart>>,
    pub(crate) timers: RefCell<BTreeMap<u32, VirtualTimer>>,
    pub(crate) gpios: RefCell<BTreeMap<u32, VirtualGpio>>,
    pub(crate) i2cs: RefCell<BTreeMap<u32, VirtualI2c>>,
    pub(crate) spis: RefCell<BTreeMap<u32, VirtualSpi>>,
    pub(crate) cans: RefCell<BTreeMap<u32, VirtualCan>>,
    pub(crate) bt_ctrls: RefCell<BTreeMap<u32, VirtualHciController>>,
    pub(crate) adcs: RefCell<BTreeMap<u32, VirtualAdc>>,
    pub(crate) temp_sensors: RefCell<BTreeMap<u32, VirtualTempSensor>>,
    pub(crate) entropy_sources: RefCell<BTreeMap<u32, VirtualEntropy>>,
    pub(crate) eeproms: RefCell<BTreeMap<u32, VirtualEeprom>>,
    pub(crate) flashes: RefCell<BTreeMap<u32, VirtualFlash>>,
    pub(crate) blocks: RefCell<BTreeMap<u32, FlatMemoryStore>>,
    pub(crate) displays: RefCell<BTreeMap<u32, VirtualDisplay>>,
    pub(crate) touches: RefCell<BTreeMap<u32, VirtualTouchScreen>>,
    pub(crate) fault_injector: RefCell<FaultInjector>,
}

impl DeviceBank {
    /// Create an empty device bank.  `const` so it can back a `const`
    /// thread-local initializer.
    pub const fn new() -> Self {
        Self {
            uarts: RefCell::new(BTreeMap::new()),
            timers: RefCell::new(BTreeMap::new()),
            gpios: RefCell::new(BTreeMap::new()),
            i2cs: RefCell::new(BTreeMap::new()),
            spis: RefCell::new(BTreeMap::new()),
            cans: RefCell::new(BTreeMap::new()),
            bt_ctrls: RefCell::new(BTreeMap::new()),
            adcs: RefCell::new(BTreeMap::new()),
            temp_sensors: RefCell::new(BTreeMap::new()),
            entropy_sources: RefCell::new(BTreeMap::new()),
            eeproms: RefCell::new(BTreeMap::new()),
            flashes: RefCell::new(BTreeMap::new()),
            blocks: RefCell::new(BTreeMap::new()),
            displays: RefCell::new(BTreeMap::new()),
            touches: RefCell::new(BTreeMap::new()),
            fault_injector: RefCell::new(FaultInjector::new()),
        }
    }

    /// Activate this bank on the current thread.  While the returned guard is
    /// alive, all `sim_devices::with_*` accessors resolve into *this* bank's
    /// devices.  The guard restores the previously active bank (or the default
    /// fallback) on drop, including on panic unwind.
    pub fn activate(&self) -> BankGuard<'_> {
        activate_bank(self)
    }
}

impl Default for DeviceBank {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    /// Fallback bank used when no [`DeviceBank`] has been activated.  Preserves
    /// the legacy single-store-per-thread behavior so existing code is
    /// byte-identical.
    static DEFAULT_BANK: DeviceBank = const { DeviceBank::new() };

    /// Pointer to the currently active bank, if any.  A `Cell` of a raw pointer
    /// — never the storage itself.
    static ACTIVE_BANK: Cell<Option<*const DeviceBank>> = const { Cell::new(None) };
}

/// Resolve the currently active device bank and run `f` against it.
///
/// This is the single access point for the per-type accessor functions.  When a
/// bank is active it is used; otherwise the thread-local [`DEFAULT_BANK`] is.
#[inline]
pub fn with_bank<F, R>(f: F) -> R
where
    F: FnOnce(&DeviceBank) -> R,
{
    ACTIVE_BANK.with(|active| {
        if let Some(ptr) = active.get() {
            // Safety: the pointer was set by `activate_bank` from a `&DeviceBank`
            // whose borrow is tied to the still-live `BankGuard` on the stack
            // above us, so the referent outlives this call.
            let bank = unsafe { &*ptr };
            f(bank)
        } else {
            DEFAULT_BANK.with(|b| f(b))
        }
    })
}

/// Activate `bank` for the current thread, returning a guard that restores the
/// previous active bank on drop.
///
/// The guard borrows `bank`, so the borrow checker prevents `bank` from being
/// dropped or moved while the guard is alive.
pub fn activate_bank(bank: &DeviceBank) -> BankGuard<'_> {
    let ptr: *const DeviceBank = bank;
    let old = ACTIVE_BANK.with(|active| active.replace(Some(ptr)));
    BankGuard {
        old,
        _phantom: PhantomData,
    }
}

/// RAII guard returned by [`activate_bank`] / [`DeviceBank::activate`].  On drop
/// it restores the bank that was active before activation.
pub struct BankGuard<'a> {
    old: Option<*const DeviceBank>,
    _phantom: PhantomData<&'a DeviceBank>,
}

impl Drop for BankGuard<'_> {
    fn drop(&mut self) {
        ACTIVE_BANK.with(|active| active.set(self.old));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CanFrame, VirtualCan};

    /// Two banks on one thread, each using CAN controller id 0, must not observe
    /// each other's frames — the P0a exit test.
    #[test]
    fn two_banks_can_id_zero_do_not_leak() {
        let bank_a = DeviceBank::new();
        let bank_b = DeviceBank::new();

        // World A: controller 0 sends frame 0xA1.
        {
            let _g = bank_a.activate();
            crate::can_insert(VirtualCan::new(0, 500_000));
            let ok =
                crate::with_can_mut(0, |c| c.send(CanFrame::new_data(0xA1, &[1, 2, 3]))).unwrap();
            assert!(ok);
        }

        // World B: controller 0 sends a *different* frame 0xB2.
        {
            let _g = bank_b.activate();
            crate::can_insert(VirtualCan::new(0, 500_000));
            let ok = crate::with_can_mut(0, |c| c.send(CanFrame::new_data(0xB2, &[9]))).unwrap();
            assert!(ok);
            // B sees exactly its own single queued frame.
            let tx_len = crate::with_can(0, |c| c.tx_queue.len()).unwrap();
            assert_eq!(tx_len, 1, "bank B controller 0 should hold only its frame");
        }

        // Back in World A: its controller 0 still holds only frame 0xA1 — bank B
        // never touched it.
        {
            let _g = bank_a.activate();
            let (len, id) = crate::with_can(0, |c| (c.tx_queue.len(), c.tx_queue[0].id)).unwrap();
            assert_eq!(len, 1, "bank A controller 0 should be untouched by bank B");
            assert_eq!(
                id, 0xA1,
                "bank A must still see its own frame, not bank B's"
            );
        }
    }

    /// The default fallback bank is distinct from any explicitly-created bank.
    #[test]
    fn explicit_bank_is_isolated_from_default() {
        // Default store (no active bank): insert timer id 0.
        crate::timer_insert(VirtualTimer::new_oneshot(0, 16));
        let default_ids = crate::timer_ids();
        assert!(default_ids.contains(&0));

        // A fresh explicit bank sees no timers.
        let bank = DeviceBank::new();
        let _g = bank.activate();
        assert!(
            crate::timer_ids().is_empty(),
            "fresh bank must not see the default store's timers"
        );
    }

    /// Device inspection (`collect_all`) is bank-scoped: it reports the active
    /// bank's devices, not another bank's.
    #[test]
    fn inspection_is_bank_scoped() {
        use crate::inspect::DeviceSnapshot;

        let bank_a = DeviceBank::new();
        let bank_b = DeviceBank::new();

        {
            let _g = bank_a.activate();
            crate::uart_insert(VirtualUart::new(0, 115200));
            crate::can_insert(VirtualCan::new(0, 500_000));
            let snaps = DeviceSnapshot::collect_all();
            assert_eq!(snaps.len(), 2, "bank A should report its 2 devices");
        }
        {
            let _g = bank_b.activate();
            // Bank B is empty despite bank A having devices at the same ids.
            let snaps = DeviceSnapshot::collect_all();
            assert!(snaps.is_empty(), "bank B should report no devices");
        }
    }

    /// Nested activation restores the outer bank when the inner guard drops.
    #[test]
    fn nested_activation_restores_outer() {
        let outer = DeviceBank::new();
        let inner = DeviceBank::new();

        let _g_outer = outer.activate();
        crate::uart_insert(VirtualUart::new(7, 9600));
        {
            let _g_inner = inner.activate();
            assert!(crate::uart_ids().is_empty(), "inner bank is empty");
            crate::uart_insert(VirtualUart::new(42, 9600));
            assert_eq!(crate::uart_ids(), vec![42]);
        }
        // Inner guard dropped: outer bank is active again with its own device.
        assert_eq!(crate::uart_ids(), vec![7]);
    }

    /// A panic while a bank is active must restore the previous context so a
    /// sibling world still runs.
    #[test]
    fn panic_restores_previous_context() {
        let bank = DeviceBank::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = bank.activate();
            crate::uart_insert(VirtualUart::new(1, 9600));
            panic!("boom inside active bank");
        }));
        assert!(result.is_err());
        // After the panic unwound the guard, no bank is active: the default
        // store is in effect and does not contain the panicking bank's device.
        assert!(
            !crate::uart_ids().contains(&1),
            "panicking bank's device must not leak into the default store"
        );
    }
}
