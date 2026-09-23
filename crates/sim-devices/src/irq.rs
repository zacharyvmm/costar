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
//! The `IrqController` lives in the active [`DeviceBank`](crate::bank::DeviceBank)
//! (falling back to the thread-local default bank), so — like the other virtual
//! devices — it can be accessed from within a running fiber (unlike `SIM_GLOBAL`
//! whose `RefCell` must not be held across fiber resume) and is scoped per-World
//! when a bank is active.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Unbounded};

use sim_core::time::Tick;

// ---------------------------------------------------------------------------
// Accessors (backed by the active DeviceBank)
// ---------------------------------------------------------------------------

/// Access the IRQ controller immutably.
pub fn with_irq<F, R>(f: F) -> R
where
    F: FnOnce(&IrqController) -> R,
{
    crate::bank::with_bank(|b| {
        let ctrl = b.inner.irq_ctrl.borrow();
        f(&ctrl)
    })
}

/// Access the IRQ controller mutably.
pub fn with_irq_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut IrqController) -> R,
{
    crate::bank::with_bank(|b| {
        let mut ctrl = b.inner.irq_ctrl.borrow_mut();
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
    /// Interrupts that have arrived and wait to be taken.
    pending: BTreeSet<u32>,
    /// Maximum number of IRQ lines supported.
    max_irqs: u32,
    /// Trace flag: whether to record IRQ events.
    pub tracing: bool,
    /// ISR registered for each IRQ line.
    handlers: BTreeMap<u32, IrqHandler>,
    /// Interrupts that arrive at a known tick, per line (never an empty
    /// set), kept apart from `pending` so that raising the same line now is
    /// not delayed and taking or acknowledging it does not drop a later
    /// arrival.
    scheduled: BTreeMap<u32, BTreeSet<Tick>>,
}

/// An interrupt service routine registered by guest firmware.
pub type IrqHandler = unsafe extern "C" fn();

impl IrqController {
    /// Create a new interrupt controller.
    pub const fn new() -> Self {
        Self {
            pending: BTreeSet::new(),
            max_irqs: 64,
            tracing: false,
            handlers: BTreeMap::new(),
            scheduled: BTreeMap::new(),
        }
    }

    /// Create with a specific maximum IRQ count.
    pub fn with_max_irqs(max_irqs: u32) -> Self {
        Self {
            pending: BTreeSet::new(),
            max_irqs,
            tracing: false,
            handlers: BTreeMap::new(),
            scheduled: BTreeMap::new(),
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

    /// Raise a virtual interrupt that arrives at tick `at`, for a device
    /// model that knows when its input arrives.  It is not taken before then
    /// (see [`take_next_due`](Self::take_next_due)); a plain
    /// [`raise`](Self::raise) is due at once.
    pub fn raise_at(&mut self, irq: u32, at: Tick) {
        if irq < self.max_irqs {
            self.schedule(irq, at);
        }
    }

    fn schedule(&mut self, irq: u32, at: Tick) {
        self.scheduled.entry(irq).or_default().insert(at);
    }

    /// Make every IRQ raised without an arrival time arrive at tick `at`.
    ///
    /// A step-driven machine calls this with the step's limit: input staged
    /// between steps arrives at the World's current instant.  IRQs raised
    /// afterwards are due at once.
    pub fn stamp_arrivals(&mut self, at: Tick) {
        for irq in std::mem::take(&mut self.pending) {
            self.schedule(irq, at);
        }
    }

    /// Earliest scheduled arrival after tick `now`.
    pub fn next_arrival_after(&self, now: Tick) -> Option<Tick> {
        self.scheduled
            .values()
            .filter_map(|arrivals| arrivals.range((Excluded(now), Unbounded)).next())
            .copied()
            .min()
    }

    /// Clear an interrupt that has arrived by tick `now` (e.g., acknowledged
    /// by the handler), whether or not it has been taken from the schedule
    /// yet.  A later arrival on the same line is kept: it has not happened.
    pub fn clear(&mut self, irq: u32, now: Tick) -> bool {
        let arrived = self.pending.remove(&irq);
        self.remove_arrived(irq, now) || arrived
    }

    /// Remove the arrivals on `irq` due by tick `now`; true if there were any.
    fn remove_arrived(&mut self, irq: u32, now: Tick) -> bool {
        let Some(arrivals) = self.scheduled.get_mut(&irq) else {
            return false;
        };
        let before = arrivals.len();
        arrivals.retain(|&at| at > now);
        let removed = arrivals.len() != before;
        if arrivals.is_empty() {
            self.scheduled.remove(&irq);
        }
        removed
    }

    /// Check whether a specific IRQ is pending or scheduled.
    pub fn is_pending(&self, irq: u32) -> bool {
        self.pending.contains(&irq) || self.scheduled.contains_key(&irq)
    }

    /// Whether any IRQs are pending or scheduled.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty() || !self.scheduled.is_empty()
    }

    /// Number of IRQ lines pending or scheduled.
    pub fn pending_count(&self) -> usize {
        self.all_pending().len()
    }

    /// Lowest-numbered IRQ that has arrived by tick `now`, without taking it.
    pub fn first_due(&self, now: Tick) -> Option<u32> {
        let scheduled = self
            .scheduled
            .iter()
            .filter(|(_, arrivals)| arrivals.first().is_some_and(|&at| at <= now))
            .map(|(&irq, _)| irq);
        self.pending.iter().copied().chain(scheduled).min()
    }

    /// Pending and scheduled IRQs.
    fn all_pending(&self) -> BTreeSet<u32> {
        self.pending
            .iter()
            .chain(self.scheduled.keys())
            .copied()
            .collect()
    }

    /// Register (or, with `None`, remove) the ISR for `irq`.
    pub fn set_handler(&mut self, irq: u32, handler: Option<IrqHandler>) {
        match handler {
            Some(h) if irq < self.max_irqs => {
                self.handlers.insert(irq, h);
            }
            _ => {
                self.handlers.remove(&irq);
            }
        }
    }

    /// The ISR registered for `irq`, if any.
    pub fn handler(&self, irq: u32) -> Option<IrqHandler> {
        self.handlers.get(&irq).copied()
    }

    /// Take the lowest-numbered (highest-priority) pending IRQ that has
    /// arrived by tick `now`.
    pub fn take_next_due(&mut self, now: Tick) -> Option<u32> {
        let lines: Vec<u32> = self.scheduled.keys().copied().collect();
        for irq in lines {
            if self.remove_arrived(irq, now) {
                self.pending.insert(irq);
            }
        }
        self.pending.pop_first()
    }

    /// Take all pending IRQs (removes them from the controller).
    ///
    /// Returns the list in priority order (ascending IRQ number = lowest first).
    /// The caller is responsible for delivering these IRQs.
    pub fn take_pending(&mut self) -> Vec<u32> {
        let irqs = self.peek_pending();
        self.pending.clear();
        self.scheduled.clear();
        irqs
    }

    /// Peek at all pending IRQs without removing them.
    pub fn peek_pending(&self) -> Vec<u32> {
        self.all_pending().into_iter().collect()
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
    fn test_irq_is_not_taken_before_it_arrives() {
        let mut ctrl = IrqController::new();
        ctrl.raise_at(4, 5);
        ctrl.raise(9);
        assert_eq!(ctrl.next_arrival_after(0), Some(5));
        // IRQ 9 has no arrival time: due at once, although 4 has priority.
        assert_eq!(ctrl.take_next_due(0), Some(9));
        assert_eq!(ctrl.take_next_due(4), None);
        assert_eq!(ctrl.take_next_due(5), Some(4));
        assert!(!ctrl.has_pending());

        // Staged without a time, then stamped with the step's limit.
        ctrl.raise(2);
        ctrl.stamp_arrivals(8);
        assert_eq!(ctrl.take_next_due(7), None);
        // Raising it again at an earlier tick moves its arrival forward.
        ctrl.raise_at(2, 6);
        assert_eq!(ctrl.take_next_due(7), Some(2));
    }

    #[test]
    fn test_raise_now_is_not_delayed_by_a_scheduled_arrival() {
        let mut ctrl = IrqController::new();
        ctrl.raise_at(6, 5);
        ctrl.raise(6);
        assert_eq!(ctrl.take_next_due(2), Some(6));
        // The scheduled arrival still comes, at its own time.
        assert_eq!(ctrl.take_next_due(4), None);
        assert_eq!(ctrl.next_arrival_after(2), Some(5));
        assert_eq!(ctrl.take_next_due(5), Some(6));
        assert!(!ctrl.has_pending());

        // Same for input stamped with a step's limit.
        ctrl.raise(3);
        ctrl.stamp_arrivals(20);
        ctrl.raise(3);
        assert_eq!(ctrl.pending_count(), 1);
        assert_eq!(ctrl.take_next_due(7), Some(3));
        assert_eq!(ctrl.take_next_due(19), None);
        assert_eq!(ctrl.take_next_due(20), Some(3));
    }

    #[test]
    fn test_acknowledging_an_irq_keeps_a_scheduled_arrival() {
        let mut ctrl = IrqController::new();
        ctrl.raise_at(6, 20);
        ctrl.raise(6);
        assert_eq!(ctrl.first_due(7), Some(6));
        assert!(ctrl.clear(6, 7));
        // Only the arrived interrupt was acknowledged.
        assert_eq!(ctrl.first_due(7), None);
        assert_eq!(ctrl.next_arrival_after(7), Some(20));
        assert!(!ctrl.clear(6, 7));
        assert_eq!(ctrl.first_due(20), Some(6));
        assert_eq!(ctrl.take_next_due(20), Some(6));
        assert!(!ctrl.has_pending());
    }

    #[test]
    fn test_acknowledging_a_due_arrival_before_it_is_taken() {
        // While interrupts are masked, nothing takes the arrival at 5, but
        // it has arrived: acknowledging it at 5 clears it, and the arrival
        // at 9 on the same line is kept.
        let mut ctrl = IrqController::new();
        ctrl.raise_at(6, 5);
        ctrl.raise_at(6, 9);
        assert_eq!(ctrl.first_due(5), Some(6));
        assert!(ctrl.clear(6, 5));
        assert_eq!(ctrl.first_due(5), None);
        assert_eq!(ctrl.take_next_due(8), None);
        assert_eq!(ctrl.next_arrival_after(5), Some(9));
        assert_eq!(ctrl.take_next_due(9), Some(6));
        assert!(!ctrl.has_pending());
    }

    #[test]
    fn test_arrivals_on_one_line_are_kept_apart() {
        let mut ctrl = IrqController::new();
        ctrl.raise_at(6, 9);
        ctrl.raise_at(6, 5);
        assert_eq!(ctrl.next_arrival_after(0), Some(5));
        assert_eq!(ctrl.take_next_due(5), Some(6));
        assert_eq!(ctrl.next_arrival_after(5), Some(9));
        assert_eq!(ctrl.take_next_due(9), Some(6));
        // Arrivals that are both due by the time they are taken merge into
        // one pending interrupt, like a level-triggered line.
        ctrl.raise_at(3, 5);
        ctrl.raise_at(3, 7);
        assert_eq!(ctrl.take_next_due(8), Some(3));
        assert_eq!(ctrl.take_next_due(8), None);
        assert!(!ctrl.has_pending());
    }

    #[test]
    fn test_raise_and_clear() {
        let mut ctrl = IrqController::new();
        assert!(!ctrl.has_pending());

        ctrl.raise(5);
        assert!(ctrl.has_pending());
        assert!(ctrl.is_pending(5));
        assert!(!ctrl.is_pending(3));

        ctrl.clear(5, 0);
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
        assert!(!ctrl.clear(99, 0));
        assert!(!ctrl.has_pending());
    }
}
