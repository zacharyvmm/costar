//! # sim-devices
//!
//! Deterministic virtual device models for costar.
//!
//! This crate provides:
//! * [`VirtualUart`] — UART with TX/RX buffers, trace-backed writes
//! * [`VirtualGpio`] — GPIO port with configurable pins and IRQ-on-change
//! * [`VirtualTimer`] — one-shot / periodic virtual timer that raises IRQs
//! * [`IrqController`] — interrupt controller with pending-IRQ tracking
//! * [`registry`] — compile-time driver registration via `inventory`
//!
//! # Thread-local device storage
//!
//! Device instances are stored in per-type thread-local maps keyed by
//! device ID.  C FFI functions (in sim-ffi) access them via the helper
//! functions exported here.

pub mod gpio;
pub mod irq;
pub mod registry;
pub mod timer;
pub mod uart;

pub use gpio::{GpioMode, GpioPin, VirtualGpio};
pub use irq::IrqController;
pub use registry::{init_all_drivers, SimulatedDriver};
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
