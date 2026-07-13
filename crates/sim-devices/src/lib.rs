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
pub use bank::{activate_bank, with_bank, BankGuard, DeviceBank};

/// Re-export for driver registration convenience.
pub use inventory;

// ---------------------------------------------------------------------------
// Device registry macro
// ---------------------------------------------------------------------------
//
// Every device type (UART, Timer, GPIO, I2C, SPI, CAN, BT, ADC, TempSensor,
// Entropy, EEPROM, Flash, Block, Display, Touch) needs the same four accessor
// functions.  The instances live in the active [`DeviceBank`] (see `bank`);
// this macro generates accessors that resolve into that bank via
// [`bank::with_bank`], so device state is per-World rather than a single
// process-/thread-global map.

macro_rules! device_registry {
    ($type:ty, $field:ident, $insert:ident, $with_mut:ident, $with:ident, $ids:ident) => {
        #[allow(missing_docs)]
        pub fn $insert(item: $type) {
            $crate::bank::with_bank(|b| {
                b.inner.$field.borrow_mut().insert(item.id, item);
            });
        }

        #[allow(missing_docs)]
        pub fn $with_mut<F, R>(id: u32, f: F) -> Option<R>
        where
            F: FnOnce(&mut $type) -> R,
        {
            $crate::bank::with_bank(|b| {
                let mut m = b.inner.$field.borrow_mut();
                m.get_mut(&id).map(f)
            })
        }

        #[allow(missing_docs)]
        pub fn $with<F, R>(id: u32, f: F) -> Option<R>
        where
            F: FnOnce(&$type) -> R,
        {
            $crate::bank::with_bank(|b| {
                let m = b.inner.$field.borrow();
                m.get(&id).map(f)
            })
        }

        #[allow(missing_docs)]
        pub fn $ids() -> Vec<u32> {
            $crate::bank::with_bank(|b| b.inner.$field.borrow().keys().copied().collect())
        }
    };
}

// ---------------------------------------------------------------------------
// Device registries
// ---------------------------------------------------------------------------

// ── UART ──────────────────────────────────────────────────────────────────

device_registry!(
    VirtualUart,
    uarts,
    uart_insert,
    with_uart_mut,
    with_uart,
    uart_ids
);

// ── Timer ─────────────────────────────────────────────────────────────────

device_registry!(
    VirtualTimer,
    timers,
    timer_insert,
    with_timer_mut,
    with_timer,
    timer_ids
);

/// Drain all expired timers: for each armed timer whose `next_expiry` has
/// passed, call its `fire()` method.  Returns the number of timers fired.
pub fn drain_expired_timers(now: sim_core::time::Tick) -> usize {
    bank::with_bank(|b| {
        let mut m = b.inner.timers.borrow_mut();
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
    gpios,
    gpio_insert,
    with_gpio_mut,
    with_gpio,
    gpio_ids
);

// ── I2C ───────────────────────────────────────────────────────────────────

device_registry!(
    VirtualI2c,
    i2cs,
    i2c_insert,
    with_i2c_mut,
    with_i2c,
    i2c_ids
);

// ── SPI ───────────────────────────────────────────────────────────────────

device_registry!(
    VirtualSpi,
    spis,
    spi_insert,
    with_spi_mut,
    with_spi,
    spi_ids
);

// ── CAN ───────────────────────────────────────────────────────────────────

device_registry!(
    VirtualCan,
    cans,
    can_insert,
    with_can_mut,
    with_can,
    can_ids
);

// ── Bluetooth HCI ─────────────────────────────────────────────────────────

device_registry!(
    VirtualHciController,
    bt_ctrls,
    bt_insert,
    with_bt_mut,
    with_bt,
    bt_ids
);

// ── ADC ───────────────────────────────────────────────────────────────────

device_registry!(
    VirtualAdc,
    adcs,
    adc_insert,
    with_adc_mut,
    with_adc,
    adc_ids
);

// ── Temperature sensor ────────────────────────────────────────────────────

device_registry!(
    VirtualTempSensor,
    temp_sensors,
    temp_sensor_insert,
    with_temp_sensor_mut,
    with_temp_sensor,
    temp_sensor_ids
);

// ── Entropy ───────────────────────────────────────────────────────────────

device_registry!(
    VirtualEntropy,
    entropy_sources,
    entropy_insert,
    with_entropy_mut,
    with_entropy,
    entropy_ids
);

// ── Fault injector (singleton, not BTreeMap-backed) ───────────────────────

/// Run a closure with mutable access to the active bank's fault injector.
///
/// The fault injector now lives in the active [`DeviceBank`], so fault
/// injection is per-World rather than process-global.
pub fn with_fault_injector_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut FaultInjector) -> R,
{
    bank::with_bank(|b| {
        let mut fi = b.inner.fault_injector.borrow_mut();
        f(&mut fi)
    })
}

// ── EEPROM ────────────────────────────────────────────────────────────────

device_registry!(
    VirtualEeprom,
    eeproms,
    eeprom_insert,
    with_eeprom_mut,
    with_eeprom,
    eeprom_ids
);

// ── Flash ─────────────────────────────────────────────────────────────────

device_registry!(
    VirtualFlash,
    flashes,
    flash_insert,
    with_flash_mut,
    with_flash,
    flash_ids
);

// ── Block device ──────────────────────────────────────────────────────────

device_registry!(
    FlatMemoryStore,
    blocks,
    block_insert,
    with_block_mut,
    with_block,
    block_ids
);

// ── Display ───────────────────────────────────────────────────────────────

device_registry!(
    VirtualDisplay,
    displays,
    display_insert,
    with_display_mut,
    with_display,
    display_ids
);

// ── Touch screen ──────────────────────────────────────────────────────────

device_registry!(
    VirtualTouchScreen,
    touches,
    touch_insert,
    with_touch_mut,
    with_touch,
    touch_ids
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
