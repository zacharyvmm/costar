//! Virtual timer peripheral.
//!
//! A `VirtualTimer` fires a virtual interrupt after a configurable delay.
//! It supports both one-shot and periodic modes.
//!
//! The timer integrates with the interrupt controller by calling
//! `IrqController::raise()` when it expires.  The actual time-based
//! scheduling is done via the `EventQueue` from sim-core.

use sim_core::event_queue::EventId;
use sim_core::time::Tick;

use crate::irq;

/// A virtual timer channel.
#[derive(Debug, Clone)]
pub struct VirtualTimer {
    /// Timer channel ID.
    pub id: u32,
    /// IRQ number this timer fires when it expires.
    pub irq: u32,
    /// Period in ticks.  `None` means one-shot.
    pub period: Option<Tick>,
    /// Absolute virtual time of the next expiry, if armed.
    pub next_expiry: Option<Tick>,
    /// Whether the timer is currently armed and counting.
    pub armed: bool,
    /// Number of times this timer has fired.
    pub fire_count: u64,
    /// Virtual time of the most recent fire, if any.
    pub last_fire_tick: Option<Tick>,
    /// The event queue ID of the pending expiry event, if any.
    pub event_id: Option<EventId>,
}

impl VirtualTimer {
    /// Create a new one-shot timer.
    pub fn new_oneshot(id: u32, irq: u32) -> Self {
        Self {
            id,
            irq,
            period: None,
            next_expiry: None,
            armed: false,
            fire_count: 0,
            last_fire_tick: None,
            event_id: None,
        }
    }

    /// Create a new periodic timer.
    pub fn new_periodic(id: u32, irq: u32, period: Tick) -> Self {
        Self {
            id,
            irq,
            period: Some(period),
            next_expiry: None,
            armed: false,
            fire_count: 0,
            last_fire_tick: None,
            event_id: None,
        }
    }

    /// Arm the timer to fire after `delay` ticks from `now`.
    ///
    /// If the timer was already armed, the previous schedule is cancelled.
    pub fn arm(&mut self, now: Tick, delay: Tick) {
        self.next_expiry = Some(now.saturating_add(delay));
        self.armed = true;
    }

    /// Disarm the timer.  No interrupt will fire.
    pub fn disarm(&mut self) {
        self.armed = false;
        self.next_expiry = None;
        self.event_id = None;
    }

    /// Fire the timer: raise the associated IRQ and optionally re-arm for
    /// periodic mode.  Returns `true` if the timer was actually armed.
    pub fn fire(&mut self, now: Tick) -> bool {
        if !self.armed {
            return false;
        }

        self.fire_count = self.fire_count.saturating_add(1);
        self.last_fire_tick = Some(now);

        // Raise the IRQ.
        irq::with_irq_mut(|ctrl| {
            ctrl.raise(self.irq);
        });

        // Handle re-arming.
        if let Some(period) = self.period {
            // Periodic: re-arm for next period.
            self.next_expiry = Some(now.saturating_add(period));
            // armed stays true
        } else {
            // One-shot: disarm after firing.
            self.armed = false;
            self.next_expiry = None;
            self.event_id = None;
        }

        true
    }

    /// Check whether the timer needs to fire at `now`.
    /// Does NOT actually fire — just checks expiry.
    pub fn is_expired(&self, now: Tick) -> bool {
        if !self.armed {
            return false;
        }
        match self.next_expiry {
            Some(expiry) => now >= expiry,
            None => false,
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
    fn test_oneshot_timer_fires_once() {
        let mut timer = VirtualTimer::new_oneshot(0, 16);

        assert!(!timer.armed);
        timer.arm(0, 10);
        assert!(timer.armed);
        assert_eq!(timer.next_expiry, Some(10));

        // Not expired yet
        assert!(!timer.is_expired(5));

        // Fire at time 10
        assert!(timer.fire(10));
        assert!(!timer.armed); // disarmed after one-shot
        assert_eq!(timer.fire_count, 1);
        assert_eq!(timer.last_fire_tick, Some(10));
        assert!(irq::with_irq(|c| c.is_pending(16)));

        // Clear for next test
        irq::with_irq_mut(|c| c.clear(16));
    }

    #[test]
    fn test_periodic_timer_rearms() {
        let mut timer = VirtualTimer::new_periodic(1, 17, 5);

        timer.arm(0, 10);
        assert!(timer.armed);

        // Fire at time 10
        assert!(timer.fire(10));
        assert!(timer.armed); // still armed for periodic
        assert_eq!(timer.next_expiry, Some(15)); // 10 + 5 period

        assert!(irq::with_irq(|c| c.is_pending(17)));
        irq::with_irq_mut(|c| c.clear(17));

        // Fire again at time 15
        assert!(timer.fire(15));
        assert_eq!(timer.next_expiry, Some(20)); // 15 + 5
        assert!(irq::with_irq(|c| c.is_pending(17)));
        irq::with_irq_mut(|c| c.clear(17));
    }

    #[test]
    fn test_disarm_prevents_fire() {
        let mut timer = VirtualTimer::new_oneshot(2, 18);
        timer.arm(0, 10);
        timer.disarm();
        assert!(!timer.armed);
        assert!(!timer.fire(10));
        assert!(!irq::with_irq(|c| c.is_pending(18)));
    }

    #[test]
    fn test_fire_when_not_armed_is_noop() {
        let mut timer = VirtualTimer::new_oneshot(3, 19);
        assert!(!timer.fire(10));
        assert!(!irq::with_irq(|c| c.is_pending(19)));
    }

    #[test]
    fn test_is_expired() {
        let mut timer = VirtualTimer::new_oneshot(4, 20);
        timer.arm(0, 5);
        assert!(!timer.is_expired(4));
        assert!(timer.is_expired(5));
        assert!(timer.is_expired(10)); // well past
    }

    #[test]
    fn test_rearm_overwrites_previous() {
        let mut timer = VirtualTimer::new_oneshot(5, 21);
        timer.arm(0, 100);
        assert_eq!(timer.next_expiry, Some(100));

        // Re-arm to an earlier time
        timer.arm(50, 10);
        assert_eq!(timer.next_expiry, Some(60));
    }
}
