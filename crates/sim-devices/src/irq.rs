//! Virtual interrupt controller.
//!
//! Manages virtual interrupt state: pending IRQs, raising, clearing,
//! and deferred delivery.  The controller itself does NOT check critical
//! sections — it is up to the caller (the scheduler loop or
//! `sim_exit_critical`) to call `take_pending()` only when it is safe to
//! deliver interrupts.
//!
//! # Integration with sim-ffi
//!
//! The `IrqController` is stored in a separate thread-local so it can be
//! accessed from within a running fiber (unlike `SIM_GLOBAL` whose
//! `RefCell` must not be held across fiber resume).

use std::cell::RefCell;
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Thread-local
// ---------------------------------------------------------------------------

thread_local! {
    /// The global interrupt controller instance.
    static IRQ_CTRL: RefCell<IrqController> =
        const { RefCell::new(IrqController::new()) };
}

/// Access the IRQ controller immutably.
pub fn with_irq<F, R>(f: F) -> R
where
    F: FnOnce(&IrqController) -> R,
{
    IRQ_CTRL.with(|ctrl| {
        let ctrl = ctrl.borrow();
        f(&ctrl)
    })
}

/// Access the IRQ controller mutably.
pub fn with_irq_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut IrqController) -> R,
{
    IRQ_CTRL.with(|ctrl| {
        let mut ctrl = ctrl.borrow_mut();
        f(&mut ctrl)
    })
}

// ---------------------------------------------------------------------------
// IrqController
// ---------------------------------------------------------------------------

/// A virtual interrupt controller.
///
/// Tracks pending IRQs and records raised/delivered events in the trace.
/// Does NOT handle critical-section deferral — that is the caller's
/// responsibility.
#[derive(Debug, Clone)]
pub struct IrqController {
    /// Pending interrupt numbers.
    pending: BTreeSet<u32>,
    /// Maximum number of IRQ lines supported.
    max_irqs: u32,
    /// Trace flag: whether to record IRQ events.
    pub tracing: bool,
}

impl IrqController {
    /// Create a new interrupt controller.
    pub const fn new() -> Self {
        Self {
            pending: BTreeSet::new(),
            max_irqs: 64,
            tracing: false,
        }
    }

    /// Create with a specific maximum IRQ count.
    pub fn with_max_irqs(max_irqs: u32) -> Self {
        Self {
            pending: BTreeSet::new(),
            max_irqs,
            tracing: false,
        }
    }

    /// Raise a virtual interrupt.
    ///
    /// The IRQ will be delivered the next time `take_pending()` is called
    /// from a non-critical context.
    pub fn raise(&mut self, irq: u32) {
        if irq < self.max_irqs {
            self.pending.insert(irq);
        }
    }

    /// Clear a pending interrupt (e.g., acknowledged by the handler).
    pub fn clear(&mut self, irq: u32) -> bool {
        self.pending.remove(&irq)
    }

    /// Check whether a specific IRQ is pending.
    pub fn is_pending(&self, irq: u32) -> bool {
        self.pending.contains(&irq)
    }

    /// Whether any IRQs are pending.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Number of pending IRQs.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Take all pending IRQs (removes them from the controller).
    ///
    /// Returns the list in priority order (ascending IRQ number = lowest first).
    /// The caller is responsible for delivering these IRQs.
    pub fn take_pending(&mut self) -> Vec<u32> {
        let irqs: Vec<u32> = self.pending.iter().copied().collect();
        self.pending.clear();
        irqs
    }

    /// Peek at all pending IRQs without removing them.
    pub fn peek_pending(&self) -> Vec<u32> {
        self.pending.iter().copied().collect()
    }
}

impl Default for IrqController {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raise_and_clear() {
        let mut ctrl = IrqController::new();
        assert!(!ctrl.has_pending());

        ctrl.raise(5);
        assert!(ctrl.has_pending());
        assert!(ctrl.is_pending(5));
        assert!(!ctrl.is_pending(3));

        ctrl.clear(5);
        assert!(!ctrl.has_pending());
        assert!(!ctrl.is_pending(5));
    }

    #[test]
    fn test_raise_multiple() {
        let mut ctrl = IrqController::new();
        ctrl.raise(10);
        ctrl.raise(3);
        ctrl.raise(7);

        assert_eq!(ctrl.pending_count(), 3);

        // take_pending returns in ascending order
        let irqs = ctrl.take_pending();
        assert_eq!(irqs, vec![3, 7, 10]);
        assert!(!ctrl.has_pending());
    }

    #[test]
    fn test_peek_does_not_consume() {
        let mut ctrl = IrqController::new();
        ctrl.raise(1);
        ctrl.raise(2);

        let peeked = ctrl.peek_pending();
        assert_eq!(peeked, vec![1, 2]);
        assert!(ctrl.has_pending()); // still pending
    }

    #[test]
    fn test_raise_beyond_max_is_silent_noop() {
        let mut ctrl = IrqController::with_max_irqs(4);
        ctrl.raise(2);
        ctrl.raise(5); // beyond max_irqs=4
        assert_eq!(ctrl.pending_count(), 1);
        assert!(ctrl.is_pending(2));
        assert!(!ctrl.is_pending(5));
    }

    #[test]
    fn test_clear_non_pending_is_noop() {
        let mut ctrl = IrqController::new();
        assert!(!ctrl.clear(99));
        assert!(!ctrl.has_pending());
    }
}
