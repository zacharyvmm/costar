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
//! * [`VirtualAdc`] — multi-channel ADC with configurable resolution and per-channel injected readings
//! * [`VirtualTempSensor`] — temperature sensor in millidegrees Celsius
//! * [`registry`] — compile-time driver registration via `inventory`
//!
//! # Thread-local device storage
//!
//! Device instances are stored in per-type thread-local maps keyed by
//! device ID.  C FFI functions (in sim-ffi) access them via the helper
//! functions exported here.

pub mod can;
pub mod fault;
pub mod gpio;
pub mod i2c;
pub mod irq;
pub mod registry;
pub mod sensor;
pub mod spi;
pub mod storage;
pub mod timer;
pub mod uart;

pub use can::{CanErrorState, CanFrame, VirtualCan};

pub use fault::{FaultInjector, GpioStuckFault};
pub use gpio::{GpioMode, GpioPin, VirtualGpio};
pub use i2c::VirtualI2c;
pub use irq::IrqController;
pub use registry::{init_all_drivers, SimulatedDriver};
pub use sensor::{VirtualAdc, VirtualTempSensor};
pub use spi::{SpiMode, VirtualSpi};
pub use storage::{VirtualEeprom, VirtualFlash};
pub use timer::VirtualTimer;
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

    /// Global fault injector for virtual devices.
    static FAULT_INJECTOR: RefCell<FaultInjector> =
        const { RefCell::new(FaultInjector::new()) };

    /// All registered ADC devices, keyed by ID.
    static ADCS: RefCell<BTreeMap<u32, VirtualAdc>> =
        const { RefCell::new(BTreeMap::new()) };

    /// All registered temperature sensors, keyed by ID.
    static TEMP_SENSORS: RefCell<BTreeMap<u32, VirtualTempSensor>> =
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

// ---------------------------------------------------------------------------
// C ABI exports for fault injection
// ---------------------------------------------------------------------------

/// Inject an I2C NACK fault on the next read.
///
/// # Safety
///
/// Always safe — uses thread-local fault injector storage.
#[no_mangle]
pub unsafe extern "C" fn sim_fault_inject_i2c_nack() {
    with_fault_injector_mut(|f| f.inject_i2c_nack());
}

/// Inject an SPI data/CRC error on the next transfer.
///
/// # Safety
///
/// Always safe — uses thread-local fault injector storage.
#[no_mangle]
pub unsafe extern "C" fn sim_fault_inject_spi_error() {
    with_fault_injector_mut(|f| f.inject_spi_error());
}

/// Inject a CAN bus error on the next send.
///
/// # Safety
///
/// Always safe — uses thread-local fault injector storage.
#[no_mangle]
pub unsafe extern "C" fn sim_fault_inject_can_error() {
    with_fault_injector_mut(|f| f.inject_can_error());
}

/// Clear all injected faults.
///
/// # Safety
///
/// Always safe — uses thread-local fault injector storage.
#[no_mangle]
pub unsafe extern "C" fn sim_fault_clear() {
    with_fault_injector_mut(|f| f.clear_all());
}

// ---------------------------------------------------------------------------
// C ABI exports for virtual storage (EEPROM / Flash)
// ---------------------------------------------------------------------------

/// Read a byte from a virtual EEPROM at `addr`.
///
/// Returns the byte value (0–255) on success, or `u32::MAX` if the
/// EEPROM is not found or the address is out of bounds.
///
/// # Safety
///
/// Always safe — uses thread-local EEPROM storage.
#[no_mangle]
pub unsafe extern "C" fn sim_eeprom_read(id: u32, addr: u32) -> u32 {
    with_eeprom(id, |e| e.read(addr as usize).map(|b| b as u32))
        .flatten()
        .unwrap_or(u32::MAX)
}

/// Write a byte to a virtual EEPROM at `addr`.
///
/// Returns 0 on success, 1 if the EEPROM is not found or `addr` is out
/// of bounds.
///
/// # Safety
///
/// Always safe — uses thread-local EEPROM storage.
#[no_mangle]
pub unsafe extern "C" fn sim_eeprom_write(id: u32, addr: u32, byte: u32) -> u32 {
    let success =
        with_eeprom_mut(id, |e| e.write(addr as usize, (byte & 0xFF) as u8)).unwrap_or(false);
    if success {
        0
    } else {
        1
    }
}

/// Return the size of a virtual EEPROM in bytes.
///
/// Returns the size, or 0 if the EEPROM is not found.
///
/// # Safety
///
/// Always safe — uses thread-local EEPROM storage.
#[no_mangle]
pub unsafe extern "C" fn sim_eeprom_size(id: u32) -> u32 {
    with_eeprom(id, |e| e.size as u32).unwrap_or(0)
}

/// Read a byte from a virtual Flash device at `addr`.
///
/// Returns the byte value (0–255) on success, or `u32::MAX` if the
/// Flash device is not found or the address is out of bounds.
///
/// # Safety
///
/// Always safe — uses thread-local Flash storage.
#[no_mangle]
pub unsafe extern "C" fn sim_flash_read(id: u32, addr: u32) -> u32 {
    with_flash(id, |f| f.read(addr as usize).map(|b| b as u32))
        .flatten()
        .unwrap_or(u32::MAX)
}

/// Write data to a virtual Flash page.
///
/// `page` is the 0-based page index, `offset` is the byte offset within
/// the page.  Writes only succeed if all target bytes are in the erased
/// state (`0xFF`).
///
/// `data_ptr` points to the data to write and `len` specifies the number
/// of bytes.  Returns the number of bytes written on success, or 0 if
/// the Flash device is not found or the write fails.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context.
#[no_mangle]
pub unsafe extern "C" fn sim_flash_write(
    id: u32,
    page: u32,
    offset: u32,
    data_ptr: *const u8,
    len: u32,
) -> u32 {
    if data_ptr.is_null() || len == 0 {
        return 0;
    }
    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };
    let success =
        with_flash_mut(id, |f| f.write_page(page as usize, offset as usize, data)).unwrap_or(false);
    if success {
        len
    } else {
        0
    }
}

/// Erase a page of virtual Flash memory.
///
/// Fills the specified page with the erased value (`0xFF`) and
/// increments the per-page erase counter.
///
/// Returns 0 on success, 1 if the Flash device is not found or the
/// page index is out of bounds.
///
/// # Safety
///
/// Always safe — uses thread-local Flash storage.
#[no_mangle]
pub unsafe extern "C" fn sim_flash_erase(id: u32, page: u32) -> u32 {
    let success = with_flash_mut(id, |f| f.erase_page(page as usize)).unwrap_or(false);
    if success {
        0
    } else {
        1
    }
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
