//! # Fault Injection
//!
//! Deterministic fault injection for virtual device models.
//!
//! This module provides a [`FaultInjector`] that manages fault conditions for
//! virtual devices, allowing test scripts to deliberately trigger error
//! conditions such as I2C NACKs, SPI data corruption, CAN bus errors, UART
//! framing errors, and GPIO stuck-at faults.
//!
//! # Design
//!
//! The `FaultInjector` is a single global instance (thread-local), not a
//! per-device-ID map.  Faults are "one-shot": calling `consume_*` reads
//! and resets the flag atomically from the caller's perspective.

/// Per-pin stuck-at fault masks.
#[derive(Debug, Clone, Copy, Default)]
pub struct GpioStuckFault {
    /// Bitmask of pins forced high.
    pub stuck_high: u32,
    /// Bitmask of pins forced low.
    pub stuck_low: u32,
}

/// Fault injector for virtual devices.
///
/// All faults are one-shot: once consumed (via a `consume_*` method),
/// the flag is cleared until re-injected.
#[derive(Debug, Clone, Default)]
pub struct FaultInjector {
    /// If true, the next I2C read will return a NACK.
    pub i2c_nack: bool,
    /// If true, the next SPI transfer will corrupt data.
    pub spi_error: bool,
    /// If true, the next CAN send will return a bus error.
    pub can_error: bool,
    /// If true, the next UART operation will experience a framing error.
    pub uart_framing_error: bool,
    /// GPIO stuck-at faults (per-pin mask).
    pub gpio_stuck: GpioStuckFault,
}

impl FaultInjector {
    /// Create a new fault injector with no active faults.
    pub const fn new() -> Self {
        Self {
            i2c_nack: false,
            spi_error: false,
            can_error: false,
            uart_framing_error: false,
            gpio_stuck: GpioStuckFault {
                stuck_high: 0,
                stuck_low: 0,
            },
        }
    }

    // ── Injection methods ──────────────────────────────────────────

    /// Inject an I2C NACK on the next read operation.
    pub fn inject_i2c_nack(&mut self) {
        self.i2c_nack = true;
    }

    /// Inject an SPI CRC/data error on the next transfer.
    pub fn inject_spi_error(&mut self) {
        self.spi_error = true;
    }

    /// Inject a CAN bus error on the next send.
    pub fn inject_can_error(&mut self) {
        self.can_error = true;
    }

    /// Inject a UART framing error.
    pub fn inject_uart_framing_error(&mut self) {
        self.uart_framing_error = true;
    }

    /// Force a GPIO pin stuck high.
    ///
    /// `pin` must be in 0..32.
    pub fn inject_gpio_stuck_high(&mut self, pin: u32) {
        assert!(pin < 32, "GPIO pin out of range (0..31)");
        self.gpio_stuck.stuck_high |= 1 << pin;
    }

    /// Force a GPIO pin stuck low.
    ///
    /// `pin` must be in 0..32.
    pub fn inject_gpio_stuck_low(&mut self, pin: u32) {
        assert!(pin < 32, "GPIO pin out of range (0..31)");
        self.gpio_stuck.stuck_low |= 1 << pin;
    }

    /// Clear all injected faults.
    pub fn clear_all(&mut self) {
        self.i2c_nack = false;
        self.spi_error = false;
        self.can_error = false;
        self.uart_framing_error = false;
        self.gpio_stuck.stuck_high = 0;
        self.gpio_stuck.stuck_low = 0;
    }

    // ── Consumption methods (read-and-reset) ───────────────────────

    /// Consume the I2C NACK fault.
    ///
    /// Returns `true` if a NACK was injected and not yet consumed.
    /// The flag is reset to `false` after this call.
    pub fn consume_i2c_nack(&mut self) -> bool {
        let v = self.i2c_nack;
        self.i2c_nack = false;
        v
    }

    /// Consume the SPI error fault.
    ///
    /// Returns `true` if an SPI error was injected and not yet consumed.
    /// The flag is reset to `false` after this call.
    pub fn consume_spi_error(&mut self) -> bool {
        let v = self.spi_error;
        self.spi_error = false;
        v
    }

    /// Consume the CAN error fault.
    ///
    /// Returns `true` if a CAN error was injected and not yet consumed.
    /// The flag is reset to `false` after this call.
    pub fn consume_can_error(&mut self) -> bool {
        let v = self.can_error;
        self.can_error = false;
        v
    }

    /// Consume the UART framing error fault.
    ///
    /// Returns `true` if a UART framing error was injected and not yet consumed.
    /// The flag is reset to `false` after this call.
    pub fn consume_uart_error(&mut self) -> bool {
        let v = self.uart_framing_error;
        self.uart_framing_error = false;
        v
    }

    /// Check whether a GPIO pin has a stuck-at fault.
    ///
    /// Returns:
    /// * `Some(true)` — pin is stuck high
    /// * `Some(false)` — pin is stuck low
    /// * `None` — no fault
    ///
    /// # Panics
    ///
    /// Panics if `pin >= 32`.
    pub fn check_gpio_stuck(&self, pin: u32) -> Option<bool> {
        assert!(pin < 32, "GPIO pin out of range (0..31)");
        let mask = 1u32 << pin;
        if self.gpio_stuck.stuck_high & mask != 0 {
            Some(true)
        } else if self.gpio_stuck.stuck_low & mask != 0 {
            Some(false)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_fault_injector_is_clean() {
        let fi = FaultInjector::new();
        assert!(!fi.i2c_nack);
        assert!(!fi.spi_error);
        assert!(!fi.can_error);
        assert!(!fi.uart_framing_error);
        assert_eq!(fi.gpio_stuck.stuck_high, 0);
        assert_eq!(fi.gpio_stuck.stuck_low, 0);
    }

    #[test]
    fn test_inject_and_consume_i2c_nack() {
        let mut fi = FaultInjector::new();
        assert!(!fi.consume_i2c_nack());
        fi.inject_i2c_nack();
        assert!(fi.consume_i2c_nack());
        // Consumed — should be false again
        assert!(!fi.consume_i2c_nack());
    }

    #[test]
    fn test_inject_and_consume_spi_error() {
        let mut fi = FaultInjector::new();
        assert!(!fi.consume_spi_error());
        fi.inject_spi_error();
        assert!(fi.consume_spi_error());
        assert!(!fi.consume_spi_error());
    }

    #[test]
    fn test_inject_and_consume_can_error() {
        let mut fi = FaultInjector::new();
        assert!(!fi.consume_can_error());
        fi.inject_can_error();
        assert!(fi.consume_can_error());
        assert!(!fi.consume_can_error());
    }

    #[test]
    fn test_inject_and_consume_uart_error() {
        let mut fi = FaultInjector::new();
        assert!(!fi.consume_uart_error());
        fi.inject_uart_framing_error();
        assert!(fi.consume_uart_error());
        assert!(!fi.consume_uart_error());
    }

    #[test]
    fn test_gpio_stuck_high_and_low() {
        let mut fi = FaultInjector::new();

        // No fault initially
        assert_eq!(fi.check_gpio_stuck(0), None);
        assert_eq!(fi.check_gpio_stuck(5), None);

        // Inject stuck-high on pin 3
        fi.inject_gpio_stuck_high(3);
        assert_eq!(fi.check_gpio_stuck(3), Some(true));
        assert_eq!(fi.check_gpio_stuck(0), None);

        // Inject stuck-low on pin 5
        fi.inject_gpio_stuck_low(5);
        assert_eq!(fi.check_gpio_stuck(5), Some(false));
        assert_eq!(fi.check_gpio_stuck(3), Some(true));

        // Check mask values
        assert_eq!(fi.gpio_stuck.stuck_high, 1 << 3);
        assert_eq!(fi.gpio_stuck.stuck_low, 1 << 5);
    }

    #[test]
    fn test_clear_all() {
        let mut fi = FaultInjector::new();
        fi.inject_i2c_nack();
        fi.inject_spi_error();
        fi.inject_can_error();
        fi.inject_uart_framing_error();
        fi.inject_gpio_stuck_high(0);
        fi.inject_gpio_stuck_low(1);

        assert!(fi.i2c_nack);
        assert!(fi.spi_error);
        assert!(fi.can_error);
        assert!(fi.uart_framing_error);
        assert_eq!(fi.check_gpio_stuck(0), Some(true));
        assert_eq!(fi.check_gpio_stuck(1), Some(false));

        fi.clear_all();

        assert!(!fi.i2c_nack);
        assert!(!fi.spi_error);
        assert!(!fi.can_error);
        assert!(!fi.uart_framing_error);
        assert_eq!(fi.check_gpio_stuck(0), None);
        assert_eq!(fi.check_gpio_stuck(1), None);
        assert_eq!(fi.gpio_stuck.stuck_high, 0);
        assert_eq!(fi.gpio_stuck.stuck_low, 0);
    }

    #[test]
    fn test_gpio_stuck_multiple_pins() {
        let mut fi = FaultInjector::new();
        fi.inject_gpio_stuck_high(0);
        fi.inject_gpio_stuck_high(7);
        fi.inject_gpio_stuck_low(15);
        fi.inject_gpio_stuck_low(31);

        assert_eq!(fi.check_gpio_stuck(0), Some(true));
        assert_eq!(fi.check_gpio_stuck(7), Some(true));
        assert_eq!(fi.check_gpio_stuck(15), Some(false));
        assert_eq!(fi.check_gpio_stuck(31), Some(false));
        assert_eq!(fi.check_gpio_stuck(1), None);
        assert_eq!(fi.check_gpio_stuck(16), None);
    }
}
