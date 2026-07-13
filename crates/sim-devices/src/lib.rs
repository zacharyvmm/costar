//! # sim-devices
//!
//! Deterministic virtual device models for costar.
//!
//! This crate provides:
//! * [`VirtualUart`] — UART with TX/RX buffers, trace-backed writes
//! * [`VirtualGpio`] — GPIO port with configurable pins and IRQ-on-change
//! * [`VirtualTimer`] — one-shot / periodic virtual timer that raises IRQs
//! * [`IrqController`] — interrupt controller with pending-IRQ tracking
//! * [`VirtualI2c`] — I2C controller (master mode) with TX/RX buffers
//! * [`VirtualSpi`] — SPI controller (master mode) with full-duplex transfer
//! * [`VirtualCan`] — CAN bus controller with TX/RX mailboxes, loopback mode
//! * [`VirtualEntropy`] — deterministic pseudo-random number generator
//! * [`VirtualAdc`] — multi-channel ADC with configurable resolution and per-channel injected readings
//! * [`VirtualTempSensor`] — temperature sensor in millidegrees Celsius
//! * [`registry`] — compile-time driver registration via `inventory`
//!
//! # Thread-local device storage
//!
//! Device instances are stored in per-type thread-local maps keyed by
//! device ID.  C FFI functions (in sim-ffi) access them via the helper
//! functions exported here.

#![warn(missing_docs)]

pub mod bank;
pub mod block;

pub mod bt;
pub mod can;
pub mod display;
pub mod entropy;
pub mod fault;
pub mod gpio;
pub mod i2c;
pub mod inspect;
pub mod irq;
pub mod registry;
pub mod sensor;
pub mod spi;
pub mod storage;
pub mod timer;
pub mod touch;
pub mod uart;

pub use can::{CanErrorState, CanFrame, VirtualCan};

pub use block::FlatMemoryStore;
pub use bt::{HciPacket, HciPacketType, VirtualHciController};
pub use display::{DisplayColorMode, DisplayRect, VirtualDisplay};
pub use entropy::VirtualEntropy;
pub use fault::{FaultInjector, GpioStuckFault};
pub use gpio::{GpioMode, GpioPin, VirtualGpio};
pub use i2c::VirtualI2c;
pub use irq::IrqController;
pub use registry::{init_all_drivers, SimulatedDriver};
pub use sensor::{VirtualAdc, VirtualTempSensor};
pub use spi::{SpiMode, VirtualSpi};
pub use storage::{VirtualEeprom, VirtualFlash};
pub use timer::VirtualTimer;
pub use touch::{TouchEvent, TouchEventType, VirtualTouchScreen};
pub use uart::VirtualUart;

/// Per-World device ownership.
pub use bank::{
    activate_bank, reset_volatile_devices, restore_persistent_devices, snapshot_persistent_devices,
    with_bank, BankGuard, DeviceBank, PersistentDeviceState,
};
/// Re-export for driver registration convenience.
pub use inventory;

use std::cell::RefCell;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Device registry macro
// ---------------------------------------------------------------------------
//
// Every device type (UART, Timer, GPIO, I2C, SPI, CAN, BT, ADC, TempSensor,
// Entropy, EEPROM, Flash, Block, Display, Touch) needs the same four accessor
// functions plus a thread-local BTreeMap.  This macro eliminates ~600 lines
// of copy-paste.

macro_rules! device_registry {
    ($type:ty, $static:ident, $insert_fn:ident, $with_mut_fn:ident, $with_fn:ident, $ids_fn:ident, $bank_field:ident) => {
        thread_local! {
            /// Legacy fallback store — used when no [`DeviceBank`] is active.
            /// Preserved for backward-compatible golden traces.
            static $static: RefCell<BTreeMap<u32, $type>> =
                const { RefCell::new(BTreeMap::new()) };
        }

        #[allow(missing_docs)]
        pub fn $insert_fn(item: $type) {
            let handled = bank::with_bank_if_active(|b| {
                b.inner
                    .$bank_field
                    .borrow_mut()
                    .insert(item.id, item.clone());
            });
            if handled.is_some() {
                return;
            }
            $static.with(|m| {
                m.borrow_mut().insert(item.id, item);
            });
        }
        #[allow(missing_docs)]
        pub fn $with_mut_fn<F, R>(id: u32, f: F) -> Option<R>
        where
            F: FnOnce(&mut $type) -> R,
        {
            let mut f = Some(f);
            if let Some(result) = bank::with_bank_if_active(|b| {
                b.inner
                    .$bank_field
                    .borrow_mut()
                    .get_mut(&id)
                    .map(|v| f.take().unwrap()(v))
            }) {
                return result;
            }
            $static.with(|m| {
                let mut m = m.borrow_mut();
                m.get_mut(&id).map(|v| f.take().unwrap()(v))
            })
        }

        #[allow(missing_docs)]
        pub fn $with_fn<F, R>(id: u32, f: F) -> Option<R>
        where
            F: FnOnce(&$type) -> R,
        {
            let mut f = Some(f);
            if let Some(result) = bank::with_bank_if_active(|b| {
                b.inner
                    .$bank_field
                    .borrow()
                    .get(&id)
                    .map(|v| f.take().unwrap()(v))
            }) {
                return result;
            }
            $static.with(|m| {
                let m = m.borrow();
                m.get(&id).map(|v| f.take().unwrap()(v))
            })
        }

        #[allow(missing_docs)]
        pub fn $ids_fn() -> Vec<u32> {
            if let Some(ids) = bank::with_bank_if_active(|b| {
                b.inner
                    .$bank_field
                    .borrow()
                    .keys()
                    .copied()
                    .collect::<Vec<_>>()
            }) {
                return ids;
            }
            $static.with(|m| m.borrow().keys().copied().collect())
        }
    };
}

// ---------------------------------------------------------------------------
// Device registries
// ---------------------------------------------------------------------------

// ── UART ──────────────────────────────────────────────────────────────────

device_registry!(
    VirtualUart,
    UARTS,
    uart_insert,
    with_uart_mut,
    with_uart,
    uart_ids,
    uarts
);

// ── Timer ─────────────────────────────────────────────────────────────────

device_registry!(
    VirtualTimer,
    TIMERS,
    timer_insert,
    with_timer_mut,
    with_timer,
    timer_ids,
    timers
);

/// Drain all expired timers: for each armed timer whose `next_expiry` has
/// passed, call its `fire()` method.  Returns the number of timers fired.
pub fn drain_expired_timers(now: sim_core::time::Tick) -> usize {
    TIMERS.with(|m| {
        let mut m = m.borrow_mut();
        let mut count = 0;
        for timer in m.values_mut() {
            if timer.is_expired(now) {
                timer.fire(now);
                count += 1;
            }
        }
        count
    })
}

// ── GPIO ──────────────────────────────────────────────────────────────────

device_registry!(
    VirtualGpio,
    GPIOS,
    gpio_insert,
    with_gpio_mut,
    with_gpio,
    gpio_ids,
    gpios
);

// ── I2C ───────────────────────────────────────────────────────────────────

device_registry!(
    VirtualI2c,
    I2CS,
    i2c_insert,
    with_i2c_mut,
    with_i2c,
    i2c_ids,
    i2cs
);

// ── SPI ───────────────────────────────────────────────────────────────────

device_registry!(
    VirtualSpi,
    SPIS,
    spi_insert,
    with_spi_mut,
    with_spi,
    spi_ids,
    spis
);

// ── CAN ───────────────────────────────────────────────────────────────────

device_registry!(
    VirtualCan,
    CANS,
    can_insert,
    with_can_mut,
    with_can,
    can_ids,
    cans
);

// ── Bluetooth HCI ─────────────────────────────────────────────────────────

device_registry!(
    VirtualHciController,
    BT_CTRLS,
    bt_insert,
    with_bt_mut,
    with_bt,
    bt_ids,
    bt_ctrls
);

// ── ADC ───────────────────────────────────────────────────────────────────

device_registry!(
    VirtualAdc,
    ADCS,
    adc_insert,
    with_adc_mut,
    with_adc,
    adc_ids,
    adcs
);

// ── Temperature sensor ────────────────────────────────────────────────────

device_registry!(
    VirtualTempSensor,
    TEMP_SENSORS,
    temp_sensor_insert,
    with_temp_sensor_mut,
    with_temp_sensor,
    temp_sensor_ids,
    temp_sensors
);

// ── Entropy ───────────────────────────────────────────────────────────────

device_registry!(
    VirtualEntropy,
    ENTROPY_SOURCES,
    entropy_insert,
    with_entropy_mut,
    with_entropy,
    entropy_ids,
    entropy_sources
);

// ── Fault injector (singleton, not BTreeMap-backed) ───────────────────────

thread_local! {
    /// Global fault injector for virtual devices.
    static FAULT_INJECTOR: RefCell<FaultInjector> =
        const { RefCell::new(FaultInjector::new()) };
}

/// Run a closure with mutable access to the global fault injector.
pub fn with_fault_injector_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut FaultInjector) -> R,
{
    FAULT_INJECTOR.with(|fi| {
        let mut fi = fi.borrow_mut();
        f(&mut fi)
    })
}

// ── EEPROM ────────────────────────────────────────────────────────────────

device_registry!(
    VirtualEeprom,
    EEPROMS,
    eeprom_insert,
    with_eeprom_mut,
    with_eeprom,
    eeprom_ids,
    eeproms
);

// ── Flash ─────────────────────────────────────────────────────────────────

device_registry!(
    VirtualFlash,
    FLASHES,
    flash_insert,
    with_flash_mut,
    with_flash,
    flash_ids,
    flashes
);

// ── Block device ──────────────────────────────────────────────────────────

device_registry!(
    FlatMemoryStore,
    BLOCKS,
    block_insert,
    with_block_mut,
    with_block,
    block_ids,
    blocks
);

// ── Display ───────────────────────────────────────────────────────────────

device_registry!(
    VirtualDisplay,
    DISPLAYS,
    display_insert,
    with_display_mut,
    with_display,
    display_ids,
    displays
);

// ── Touch screen ──────────────────────────────────────────────────────────

device_registry!(
    VirtualTouchScreen,
    TOUCHES,
    touch_insert,
    with_touch_mut,
    with_touch,
    touch_ids,
    touches
);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #[test]
    fn test_types_exist() {
        let _uart = super::VirtualUart::new(0, 115200);
        let _gpio = super::VirtualGpio::new(0);
        let _timer = super::VirtualTimer::new_oneshot(0, 16);
        let _irq = super::IrqController::new();
    }

    #[test]
    fn test_uart_insert_and_access() {
        super::uart_insert(super::VirtualUart::new(0, 115200));
        let exists = super::with_uart(0, |u| u.id == 0).unwrap_or(false);
        assert!(exists);

        let res = super::with_uart_mut(0, |u| {
            u.write_byte(b'X');
            u.last_tx
        });
        assert_eq!(res, Some(Some(b'X')));
    }

    #[test]
    fn test_timer_insert_and_access() {
        super::timer_insert(super::VirtualTimer::new_oneshot(0, 16));
        let res = super::with_timer_mut(0, |t| {
            t.arm(0, 10);
            t.next_expiry
        });
        assert_eq!(res, Some(Some(10)));
    }

    #[test]
    fn test_drain_expired_timers() {
        super::timer_insert(super::VirtualTimer::new_oneshot(0, 32));
        super::timer_insert(super::VirtualTimer::new_oneshot(1, 33));

        super::with_timer_mut(0, |t| t.arm(0, 5)).unwrap();
        super::with_timer_mut(1, |t| t.arm(0, 15)).unwrap();

        // At time 10, only timer 0 is expired
        let fired = super::drain_expired_timers(10);
        assert_eq!(fired, 1);
        assert!(super::irq::with_irq(|c| c.is_pending(32)));

        // Clear IRQ for next assertion
        super::irq::with_irq_mut(|c| c.clear(32));

        // At time 20, timer 1 is also expired
        let fired = super::drain_expired_timers(20);
        assert_eq!(fired, 1);
        assert!(super::irq::with_irq(|c| c.is_pending(33)));

        // Clean up
    }
}
