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

/// Re-export for driver registration convenience.
pub use inventory;

use std::cell::RefCell;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Thread-local device storage
// ---------------------------------------------------------------------------

thread_local! {
    /// All registered UART devices, keyed by ID.
    static UARTS: RefCell<BTreeMap<u32, VirtualUart>> =
        const { RefCell::new(BTreeMap::new()) };

    /// All registered timer devices, keyed by ID.
    static TIMERS: RefCell<BTreeMap<u32, VirtualTimer>> =
        const { RefCell::new(BTreeMap::new()) };

    /// All registered GPIO ports, keyed by ID.
    static GPIOS: RefCell<BTreeMap<u32, VirtualGpio>> =
        const { RefCell::new(BTreeMap::new()) };

    /// All registered I2C controllers, keyed by ID.
    static I2CS: RefCell<BTreeMap<u32, VirtualI2c>> =
        const { RefCell::new(BTreeMap::new()) };

    /// All registered SPI controllers, keyed by ID.
    static SPIS: RefCell<BTreeMap<u32, VirtualSpi>> =
        const { RefCell::new(BTreeMap::new()) };

    /// All registered CAN controllers, keyed by ID.
    static CANS: RefCell<BTreeMap<u32, VirtualCan>> =
        const { RefCell::new(BTreeMap::new()) };

    /// All registered EEPROM devices, keyed by ID.
    static EEPROMS: RefCell<BTreeMap<u32, VirtualEeprom>> =
        const { RefCell::new(BTreeMap::new()) };

    /// All registered Flash devices, keyed by ID.
    static FLASHES: RefCell<BTreeMap<u32, VirtualFlash>> =
        const { RefCell::new(BTreeMap::new()) };

    /// All registered block devices, keyed by ID.
    static BLOCKS: RefCell<BTreeMap<u32, FlatMemoryStore>> =
        const { RefCell::new(BTreeMap::new()) };

    /// Global fault injector for virtual devices.
    static FAULT_INJECTOR: RefCell<FaultInjector> =
        const { RefCell::new(FaultInjector::new()) };

    /// All registered ADC devices, keyed by ID.
    static ADCS: RefCell<BTreeMap<u32, VirtualAdc>> =
        const { RefCell::new(BTreeMap::new()) };

    /// All registered temperature sensors, keyed by ID.
    static TEMP_SENSORS: RefCell<BTreeMap<u32, VirtualTempSensor>> =
        const { RefCell::new(BTreeMap::new()) };

    /// All registered entropy sources, keyed by ID.
    static ENTROPY_SOURCES: RefCell<BTreeMap<u32, VirtualEntropy>> =
        const { RefCell::new(BTreeMap::new()) };

    /// All registered Bluetooth HCI controllers, keyed by ID.
    static BT_CTRLS: RefCell<BTreeMap<u32, VirtualHciController>> =
        const { RefCell::new(BTreeMap::new()) };

    /// All registered virtual displays, keyed by ID.
    static DISPLAYS: RefCell<BTreeMap<u32, VirtualDisplay>> =
        const { RefCell::new(BTreeMap::new()) };

    /// All registered virtual touch screens, keyed by ID.
    static TOUCHES: RefCell<BTreeMap<u32, VirtualTouchScreen>> =
        const { RefCell::new(BTreeMap::new()) };
}

// ── UART helpers ──────────────────────────────────────────────────────────

/// Insert or replace a UART device.
pub fn uart_insert(uart: VirtualUart) {
    UARTS.with(|m| {
        m.borrow_mut().insert(uart.id, uart);
    });
}

/// Run a closure with mutable access to a UART.
pub fn with_uart_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut VirtualUart) -> R,
{
    UARTS.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to a UART.
pub fn with_uart<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&VirtualUart) -> R,
{
    UARTS.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

/// Return all registered UART IDs.
pub fn uart_ids() -> Vec<u32> {
    UARTS.with(|m| m.borrow().keys().copied().collect())
}

// ── Timer helpers ──────────────────────────────────────────────────────────

/// Insert or replace a timer device.
pub fn timer_insert(timer: VirtualTimer) {
    TIMERS.with(|m| {
        m.borrow_mut().insert(timer.id, timer);
    });
}

/// Run a closure with mutable access to a timer.
pub fn with_timer_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut VirtualTimer) -> R,
{
    TIMERS.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to a timer.
pub fn with_timer<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&VirtualTimer) -> R,
{
    TIMERS.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

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

/// Return all registered timer IDs.
pub fn timer_ids() -> Vec<u32> {
    TIMERS.with(|m| m.borrow().keys().copied().collect())
}

// ── GPIO helpers ───────────────────────────────────────────────────────────

/// Insert or replace a GPIO port.
pub fn gpio_insert(gpio: VirtualGpio) {
    GPIOS.with(|m| {
        m.borrow_mut().insert(gpio.id, gpio);
    });
}

/// Run a closure with mutable access to a GPIO port.
pub fn with_gpio_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut VirtualGpio) -> R,
{
    GPIOS.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Return all registered GPIO port IDs.
pub fn gpio_ids() -> Vec<u32> {
    GPIOS.with(|m| m.borrow().keys().copied().collect())
}

// ── I2C helpers ────────────────────────────────────────────────────────────

/// Insert or replace an I2C controller.
pub fn i2c_insert(i2c: VirtualI2c) {
    I2CS.with(|m| {
        m.borrow_mut().insert(i2c.id, i2c);
    });
}

/// Run a closure with mutable access to an I2C controller.
pub fn with_i2c_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut VirtualI2c) -> R,
{
    I2CS.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to an I2C controller.
pub fn with_i2c<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&VirtualI2c) -> R,
{
    I2CS.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

/// Return all registered I2C controller IDs.
pub fn i2c_ids() -> Vec<u32> {
    I2CS.with(|m| m.borrow().keys().copied().collect())
}

// ── SPI helpers ────────────────────────────────────────────────────────────

/// Insert or replace an SPI controller.
pub fn spi_insert(spi: VirtualSpi) {
    SPIS.with(|m| {
        m.borrow_mut().insert(spi.id, spi);
    });
}

/// Run a closure with mutable access to an SPI controller.
pub fn with_spi_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut VirtualSpi) -> R,
{
    SPIS.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to an SPI controller.
pub fn with_spi<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&VirtualSpi) -> R,
{
    SPIS.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

/// Return all registered SPI controller IDs.
pub fn spi_ids() -> Vec<u32> {
    SPIS.with(|m| m.borrow().keys().copied().collect())
}

// ── CAN helpers ────────────────────────────────────────────────────────────

/// Insert or replace a CAN controller.
pub fn can_insert(can: VirtualCan) {
    CANS.with(|m| {
        m.borrow_mut().insert(can.id, can);
    });
}

/// Run a closure with mutable access to a CAN controller.
pub fn with_can_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut VirtualCan) -> R,
{
    CANS.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to a CAN controller.
pub fn with_can<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&VirtualCan) -> R,
{
    CANS.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

/// Return all registered CAN controller IDs.
pub fn can_ids() -> Vec<u32> {
    CANS.with(|m| m.borrow().keys().copied().collect())
}

// ── Bluetooth HCI helpers ──────────────────────────────────────────────────

/// Insert or replace a Bluetooth HCI controller.
pub fn bt_insert(ctrl: VirtualHciController) {
    BT_CTRLS.with(|m| {
        m.borrow_mut().insert(ctrl.id, ctrl);
    });
}

/// Run a closure with mutable access to a BT HCI controller.
pub fn with_bt_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut VirtualHciController) -> R,
{
    BT_CTRLS.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to a BT HCI controller.
pub fn with_bt<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&VirtualHciController) -> R,
{
    BT_CTRLS.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

/// Return all registered BT controller IDs.
pub fn bt_ids() -> Vec<u32> {
    BT_CTRLS.with(|m| {
        m.borrow().keys().copied().collect()
    })
}

// ── ADC helpers ────────────────────────────────────────────────────────────

/// Insert or replace an ADC device.
pub fn adc_insert(adc: VirtualAdc) {
    ADCS.with(|m| {
        m.borrow_mut().insert(adc.id, adc);
    });
}

/// Run a closure with mutable access to an ADC.
pub fn with_adc_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut VirtualAdc) -> R,
{
    ADCS.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to an ADC.
pub fn with_adc<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&VirtualAdc) -> R,
{
    ADCS.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

/// Return all registered ADC IDs.
pub fn adc_ids() -> Vec<u32> {
    ADCS.with(|m| m.borrow().keys().copied().collect())
}

// ── Temperature sensor helpers ─────────────────────────────────────────────

/// Insert or replace a temperature sensor.
pub fn temp_sensor_insert(sensor: VirtualTempSensor) {
    TEMP_SENSORS.with(|m| {
        m.borrow_mut().insert(sensor.id, sensor);
    });
}

/// Run a closure with mutable access to a temperature sensor.
pub fn with_temp_sensor_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut VirtualTempSensor) -> R,
{
    TEMP_SENSORS.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to a temperature sensor.
pub fn with_temp_sensor<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&VirtualTempSensor) -> R,
{
    TEMP_SENSORS.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

/// Return all registered temperature sensor IDs.
pub fn temp_sensor_ids() -> Vec<u32> {
    TEMP_SENSORS.with(|m| m.borrow().keys().copied().collect())
}

// ── Entropy helpers ────────────────────────────────────────────────────────

/// Insert or replace an entropy source.
pub fn entropy_insert(entropy: VirtualEntropy) {
    ENTROPY_SOURCES.with(|m| {
        m.borrow_mut().insert(entropy.id, entropy);
    });
}

/// Run a closure with mutable access to an entropy source.
pub fn with_entropy_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut VirtualEntropy) -> R,
{
    ENTROPY_SOURCES.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to an entropy source.
pub fn with_entropy<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&VirtualEntropy) -> R,
{
    ENTROPY_SOURCES.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

// ── Fault injector helpers ───────────────────────────────────────────

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

// ── EEPROM helpers ──────────────────────────────────────────────────────

/// Insert or replace an EEPROM device.
pub fn eeprom_insert(eeprom: VirtualEeprom) {
    EEPROMS.with(|m| {
        m.borrow_mut().insert(eeprom.id, eeprom);
    });
}

/// Run a closure with mutable access to an EEPROM.
pub fn with_eeprom_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut VirtualEeprom) -> R,
{
    EEPROMS.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to an EEPROM.
pub fn with_eeprom<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&VirtualEeprom) -> R,
{
    EEPROMS.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

/// Return all registered EEPROM IDs.
pub fn eeprom_ids() -> Vec<u32> {
    EEPROMS.with(|m| m.borrow().keys().copied().collect())
}

// ── Flash helpers ───────────────────────────────────────────────────────

/// Insert or replace a Flash device.
pub fn flash_insert(flash: VirtualFlash) {
    FLASHES.with(|m| {
        m.borrow_mut().insert(flash.id, flash);
    });
}

/// Run a closure with mutable access to a Flash device.
pub fn with_flash_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut VirtualFlash) -> R,
{
    FLASHES.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to a Flash device.
pub fn with_flash<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&VirtualFlash) -> R,
{
    FLASHES.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

/// Return all registered Flash device IDs.
pub fn flash_ids() -> Vec<u32> {
    FLASHES.with(|m| m.borrow().keys().copied().collect())
}

// ── Block device helpers ──────────────────────────────────────────────────

/// Insert or replace a block device.
pub fn block_insert(block: FlatMemoryStore) {
    BLOCKS.with(|m| {
        m.borrow_mut().insert(block.id, block);
    });
}

/// Run a closure with mutable access to a block device.
pub fn with_block_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut FlatMemoryStore) -> R,
{
    BLOCKS.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to a block device.
pub fn with_block<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&FlatMemoryStore) -> R,
{
    BLOCKS.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

// ── Display helpers ────────────────────────────────────────────────────────

/// Insert or replace a virtual display.
pub fn display_insert(display: VirtualDisplay) {
    DISPLAYS.with(|m| {
        m.borrow_mut().insert(display.id, display);
    });
}

/// Run a closure with mutable access to a display.
pub fn with_display_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut VirtualDisplay) -> R,
{
    DISPLAYS.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to a display.
pub fn with_display<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&VirtualDisplay) -> R,
{
    DISPLAYS.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

/// Return all registered display IDs.
pub fn display_ids() -> Vec<u32> {
    DISPLAYS.with(|m| {
        m.borrow().keys().copied().collect()
    })
}

// ── Touch screen helpers ───────────────────────────────────────────────────

/// Insert or replace a touch screen.
pub fn touch_insert(touch: VirtualTouchScreen) {
    TOUCHES.with(|m| {
        m.borrow_mut().insert(touch.id, touch);
    });
}

/// Run a closure with mutable access to a touch screen.
pub fn with_touch_mut<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut VirtualTouchScreen) -> R,
{
    TOUCHES.with(|m| {
        let mut m = m.borrow_mut();
        m.get_mut(&id).map(f)
    })
}

/// Run a closure with immutable access to a touch screen.
pub fn with_touch<F, R>(id: u32, f: F) -> Option<R>
where
    F: FnOnce(&VirtualTouchScreen) -> R,
{
    TOUCHES.with(|m| {
        let m = m.borrow();
        m.get(&id).map(f)
    })
}

/// Return all registered touch screen IDs.
pub fn touch_ids() -> Vec<u32> {
    TOUCHES.with(|m| {
        m.borrow().keys().copied().collect()
    })
}

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
        super::irq::with_irq_mut(|c| {
            c.clear(32);
            c.clear(33);
        });
    }
}
