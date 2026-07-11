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
//! accessors resolve into *its* devices.  The active stack owns a reference-
//! counted handle to every active bank, so dispatch never relies on a borrowed
//! pointer remaining valid.  This mirrors the active `SimGlobal` stack in
//! `sim-ffi`.
//!
//! # Backward compatibility (byte-identical default)
//!
//! When no bank is active, the accessors fall back to a thread-local
//! [`DEFAULT_BANK`].  Existing single-`World`-per-process code paths (which never
//! activate a bank) therefore behave exactly as before — one thread-local store,
//! single-threaded access — so golden traces stay byte-identical.  Per-world
//! isolation is opt-in: it engages only when a caller activates a bank.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::{
    FaultInjector, FlatMemoryStore, IrqController, VirtualAdc, VirtualCan, VirtualDisplay,
    VirtualEeprom, VirtualEntropy, VirtualFlash, VirtualGpio, VirtualHciController, VirtualI2c,
    VirtualSpi, VirtualTempSensor, VirtualTimer, VirtualTouchScreen, VirtualUart,
};

/// Owns one instance map per virtual-device type for a single `World`.
///
/// Each field is an independent `RefCell` so borrow granularity matches the
/// legacy per-type thread-local maps exactly — accessing one device type never
/// borrows another, so no new re-entrancy/double-borrow hazard is introduced.
///
/// The singleton [`FaultInjector`] and [`IrqController`] live here too so fault
/// injection and interrupt state are also per-world rather than process-global.
#[derive(Clone)]
pub struct DeviceBank {
    /// Shared storage for this handle and any currently active context entry.
    /// `Rc` intentionally keeps a bank thread-affine: virtual devices are
    /// single-threaded state and an active context is local to one thread.
    pub(crate) inner: Rc<DeviceBankInner>,
}

/// The mutable device storage behind a [`DeviceBank`] handle.
///
/// It is separate from the public handle so an active-context stack can retain
/// ownership without borrowing the caller's handle.  The fields remain crate
/// visible because the generated registry accessors retain their existing
/// fine-grained `RefCell` borrow behavior.
pub(crate) struct DeviceBankInner {
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
    pub(crate) irq_ctrl: RefCell<IrqController>,
}

impl DeviceBank {
    /// Create an empty device bank.
    pub fn new() -> Self {
        Self {
            inner: Rc::new(DeviceBankInner {
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
                irq_ctrl: RefCell::new(IrqController::new()),
            }),
        }
    }

    /// Run `f` with this bank active on the current thread.
    ///
    /// This is the preferred API for production code.  It establishes and
    /// restores the context in one lexical scope, including during panic
    /// unwind, so callers cannot accidentally retain a stale activation.
    pub fn with_active<R>(&self, f: impl FnOnce() -> R) -> R {
        let _guard = self.activate();
        f()
    }

    /// Activate this bank on the current thread.
    ///
    /// Prefer [`with_active`](Self::with_active) for ordinary work.  The guard
    /// remains available for compatibility with callers that must compose
    /// several activation guards.  Its active stack entry owns a clone of this
    /// bank, so non-LIFO drops and a forgotten guard cannot leave a dangling
    /// active context.
    pub fn activate(&self) -> BankGuard {
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
    static DEFAULT_BANK: DeviceBank = DeviceBank::new();

    /// Active contexts in activation order.  Every entry owns the bank it
    /// selects, which is the lifetime proof for dispatch from `with_bank`.
    static ACTIVE_BANKS: RefCell<Vec<Rc<ActiveBank>>> = const { RefCell::new(Vec::new()) };
}

struct ActiveBank {
    bank: DeviceBank,
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
    // Clone the selected handle before invoking `f`, so no borrow of the
    // thread-local stack is held across arbitrary device code.  The clone also
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

/// Activate `bank` for the current thread, returning a guard that restores the
/// previous active bank on drop.
///
/// The returned guard owns an activation-stack entry that retains the bank.
/// This is safe even if guards are dropped out of order or intentionally
/// forgotten: the stack never contains a non-owning reference.
pub fn activate_bank(bank: &DeviceBank) -> BankGuard {
    let activation = Rc::new(ActiveBank {
        bank: bank.clone(),
    });
    ACTIVE_BANKS.with(|active| active.borrow_mut().push(activation.clone()));
    BankGuard { activation }
}

/// RAII guard returned by [`activate_bank`] / [`DeviceBank::activate`].  On drop
/// it removes its own activation entry, restoring whichever context remains on
/// top of the stack.
#[must_use = "an active device context ends when its guard is dropped"]
pub struct BankGuard {
    activation: Rc<ActiveBank>,
}

impl Drop for BankGuard {
    fn drop(&mut self) {
        // Removing by identity, rather than restoring a cached previous value,
        // keeps nested contexts correct even when guards are dropped out of
        // LIFO order.  `try_with` avoids a second panic during TLS teardown.
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

    /// An outer guard may be dropped before an inner guard without restoring
    /// the removed outer context.  This is the ordering that made the former
    /// raw-pointer restoration unsafe.
    #[test]
    fn out_of_order_drop_removes_only_its_own_context() {
        std::thread::spawn(|| {
            const OUTER_ID: u32 = 0xB000;
            const INNER_ID: u32 = 0xB001;

            let outer = DeviceBank::new();
            let inner = DeviceBank::new();

            let outer_guard = outer.activate();
            crate::uart_insert(VirtualUart::new(OUTER_ID, 9_600));

            let inner_guard = inner.activate();
            crate::uart_insert(VirtualUart::new(INNER_ID, 9_600));

            // Remove and then destroy the outer owner while the inner context
            // remains active.  The inner guard must never restore `outer`.
            drop(outer_guard);
            drop(outer);
            assert!(crate::with_uart(INNER_ID, |_| ()).is_some());
            assert!(crate::with_uart(OUTER_ID, |_| ()).is_none());

            drop(inner_guard);
            drop(inner);
            assert!(crate::with_uart(INNER_ID, |_| ()).is_none());
        })
        .join()
        .expect("out-of-order activation thread must not panic");
    }

    /// Forgetting a guard intentionally leaks its active scope, but it must
    /// retain the bank rather than leaving a dangling active pointer.
    #[test]
    fn forgotten_guard_retains_its_active_bank() {
        std::thread::spawn(|| {
            const ID: u32 = 0xB002;

            let bank = DeviceBank::new();
            let guard = bank.activate();
            crate::uart_insert(VirtualUart::new(ID, 9_600));

            std::mem::forget(guard);
            drop(bank);

            assert!(
                crate::with_uart(ID, |_| ()).is_some(),
                "the leaked activation must keep its bank alive"
            );
        })
        .join()
        .expect("forgotten-guard activation thread must not panic");
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

    /// The interrupt controller is bank-scoped: an IRQ raised while one bank is
    /// active is not visible from another bank (it lives in the bank, not a
    /// process-global thread-local).
    #[test]
    fn irq_controller_is_bank_scoped() {
        let bank_a = DeviceBank::new();
        let bank_b = DeviceBank::new();

        {
            let _g = bank_a.activate();
            crate::irq::with_irq_mut(|c| c.raise(7));
            assert!(crate::irq::with_irq(|c| c.is_pending(7)));
        }
        {
            let _g = bank_b.activate();
            // Bank B's IRQ controller is independent: bank A's pending IRQ 7 is
            // not visible here.
            assert!(
                !crate::irq::with_irq(|c| c.is_pending(7)),
                "bank B must not observe bank A's pending IRQ"
            );
            crate::irq::with_irq_mut(|c| c.raise(9));
        }
        // Back in A: its IRQ 7 is still pending and it never saw B's IRQ 9.
        {
            let _g = bank_a.activate();
            assert!(crate::irq::with_irq(|c| c.is_pending(7)), "A keeps its IRQ");
            assert!(
                !crate::irq::with_irq(|c| c.is_pending(9)),
                "A must not observe bank B's IRQ"
            );
        }
    }
}
