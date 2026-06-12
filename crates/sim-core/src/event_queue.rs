//! Deterministic min-heap event queue.
//!
//! Events are ordered by (virtual timestamp, priority, insertion sequence).
//! The insertion sequence breaks ties when timestamp and priority are
//! identical, guaranteeing deterministic dispatch order across all host
//! platforms for the same seed and input.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

use crate::time::Tick;

/// An opaque event identifier.
pub type EventId = u64;

/// Ordering key for events in the min-heap.
///
/// **Lower values of every field run first.**
/// `Reverse` wrapping makes the `BinaryHeap` a min-heap.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub struct QueueKey {
    /// Absolute virtual timestamp.
    pub at: Tick,
    /// Lower priority runs first.  0 = fatal, 10 = IRQ, 20 = RTOS tick, ...
    pub priority: u16,
    /// Monotonic insertion sequence; breaks ties deterministically.
    pub seq: u64,
    /// Opaque event id (unique per event).
    pub id: EventId,
}

/// A scheduled event with an optional callback.
pub struct ScheduledEvent {
    /// Ordering key; `None` after cancellation.
    pub key: Option<QueueKey>,
    /// The callback to fire.  `None` means this event was tombstoned.
    pub callback: Option<EventCallback>,
    /// Human-readable label for tracing.
    pub label: &'static str,
}

/// Trait that event callback contexts must implement.
///
/// The concrete type lives in `run_loop::SimulatorContext`.
pub trait EventContext {
    /// Drain the guest RTOS scheduler until no progress can be made.
    fn drain_rtos_scheduler(&mut self, now: Tick) -> Result<(), crate::error::SimError>;
}

/// Type-erased event callback.
///
/// `'static` bound is required because callbacks must be storable for the
/// lifetime of the simulation run.  Use `Rc<RefCell<>>` or similar for
/// test-local state sharing.
pub type EventCallback = Box<dyn FnOnce(&mut dyn EventContext) + 'static>;

/// Deterministic event queue.
///
/// The queue owns a binary heap of `Reverse<QueueKey>` and a `BTreeMap`
/// mapping `EventId` to the full `ScheduledEvent`.  Cancellation removes
/// the map entry and leaves a tombstone in the heap; on pop, tombstoned
/// entries are skipped.
pub struct EventQueue {
    heap: BinaryHeap<Reverse<QueueKey>>,
    events: BTreeMap<EventId, ScheduledEvent>,
    next_id: EventId,
    next_seq: u64,
}

impl EventQueue {
    /// Create an empty event queue.
    pub fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            events: BTreeMap::new(),
            next_id: 1,
            next_seq: 0,
        }
    }

    /// Schedule an event at an absolute virtual timestamp.
    pub fn schedule_at(
        &mut self,
        at: Tick,
        priority: u16,
        label: &'static str,
        callback: EventCallback,
    ) -> EventId {
        let id = self.next_id;
        self.next_id += 1;
        let seq = self.next_seq;
        self.next_seq += 1;

        let key = QueueKey {
            at,
            priority,
            seq,
            id,
        };

        self.heap.push(Reverse(key));
        self.events.insert(
            id,
            ScheduledEvent {
                key: Some(key),
                callback: Some(callback),
                label,
            },
        );

        id
    }

    /// Schedule an event `delta` ticks from `now`.
    pub fn schedule_after(
        &mut self,
        now: Tick,
        delta: Tick,
        priority: u16,
        label: &'static str,
        callback: EventCallback,
    ) -> EventId {
        let at = now.saturating_add(delta);
        self.schedule_at(at, priority, label, callback)
    }

    /// Cancel a previously scheduled event.
    ///
    /// Returns `true` if the event existed and was cancelled.
    pub fn cancel(&mut self, id: EventId) -> bool {
        if let Some(event) = self.events.get_mut(&id) {
            event.key = None;
            event.callback = None;
            true
        } else {
            false
        }
    }

    /// Pop the next event that should fire.
    ///
    /// Skips tombstoned entries (cancelled events) automatically.
    pub fn pop_next(&mut self) -> Option<ScheduledEvent> {
        loop {
            let Reverse(key) = self.heap.pop()?;
            if let Some(event) = self.events.remove(&key.id) {
                if event.key.is_some() {
                    return Some(event);
                }
                // Otherwise it was cancelled; skip and continue.
            }
            // Already removed (duplicate tombstone); skip and continue.
        }
    }

    /// Peek at the timestamp of the next event without popping.
    pub fn peek_time(&self) -> Option<Tick> {
        // Walk the heap looking for the first non-tombstoned event.
        self.heap
            .iter()
            .filter_map(|Reverse(key)| {
                self.events
                    .get(&key.id)
                    .and_then(|e| if e.key.is_some() { Some(key.at) } else { None })
            })
            .min()
    }

    /// Number of live (non-cancelled) events.
    pub fn len(&self) -> usize {
        self.events.iter().filter(|(_, e)| e.key.is_some()).count()
    }

    /// Whether there are no live events.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for EventQueue {
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
    use std::cell::RefCell;
    use std::rc::Rc;

    // Mock context for tests
    struct MockContext;
    impl EventContext for MockContext {
        fn drain_rtos_scheduler(
            &mut self,
            _now: Tick,
        ) -> Result<(), crate::error::SimError> {
            Ok(())
        }
    }

    /// Push a value into a shared vec, then unwrap in the caller.
    fn push_to<T: 'static>(target: Rc<RefCell<Vec<T>>>, value: T) -> EventCallback {
        Box::new(move |_| {
            target.borrow_mut().push(value);
        })
    }

    #[test]
    fn test_same_timestamp_different_priority() {
        let mut q = EventQueue::new();
        let results: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));

        q.schedule_at(100, 30, "c", push_to(results.clone(), "c"));
        q.schedule_at(100, 10, "a", push_to(results.clone(), "a"));
        q.schedule_at(100, 20, "b", push_to(results.clone(), "b"));

        while let Some(event) = q.pop_next() {
            if let Some(cb) = event.callback {
                cb(&mut MockContext);
            }
        }

        assert_eq!(*results.borrow(), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_same_priority_insertion_order() {
        let mut q = EventQueue::new();
        let results: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));

        q.schedule_at(100, 10, "first", push_to(results.clone(), "first"));
        q.schedule_at(100, 10, "second", push_to(results.clone(), "second"));
        q.schedule_at(100, 10, "third", push_to(results.clone(), "third"));

        while let Some(event) = q.pop_next() {
            if let Some(cb) = event.callback {
                cb(&mut MockContext);
            }
        }

        assert_eq!(*results.borrow(), vec!["first", "second", "third"]);
    }

    #[test]
    fn test_different_timestamp() {
        let mut q = EventQueue::new();
        let results: Rc<RefCell<Vec<u64>>> = Rc::new(RefCell::new(Vec::new()));

        q.schedule_at(300, 10, "", push_to(results.clone(), 300));
        q.schedule_at(100, 10, "", push_to(results.clone(), 100));
        q.schedule_at(200, 10, "", push_to(results.clone(), 200));

        while let Some(event) = q.pop_next() {
            if let Some(cb) = event.callback {
                cb(&mut MockContext);
            }
        }

        assert_eq!(*results.borrow(), vec![100, 200, 300]);
    }

    #[test]
    fn test_cancellation() {
        let mut q = EventQueue::new();
        let fired: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));

        let id1 = q.schedule_at(100, 10, "a", push_to(fired.clone(), "a"));
        q.schedule_at(200, 10, "b", push_to(fired.clone(), "b"));
        q.schedule_at(300, 10, "c", push_to(fired.clone(), "c"));

        assert!(q.cancel(id1));

        while let Some(event) = q.pop_next() {
            if let Some(cb) = event.callback {
                cb(&mut MockContext);
            }
        }

        assert_eq!(*fired.borrow(), vec!["b", "c"]);
    }

    #[test]
    fn test_tombstone_skipping() {
        let mut q = EventQueue::new();
        let fired: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));

        let id = q.schedule_at(100, 10, "will_cancel", push_to(fired.clone(), "cancel"));
        q.schedule_at(200, 10, "should_fire", push_to(fired.clone(), "fire"));

        q.cancel(id);

        // pop_next should skip the tombstoned entry
        let event = q.pop_next().unwrap();
        event.callback.unwrap()(&mut MockContext);
        assert_eq!(*fired.borrow(), vec!["fire"]);
        assert!(q.pop_next().is_none());
    }

    #[test]
    fn test_empty() {
        let mut q = EventQueue::new();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
        assert!(q.pop_next().is_none());

        q.schedule_at(100, 10, "x", Box::new(|_| {}));
        assert!(!q.is_empty());
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn test_peek_time() {
        let mut q = EventQueue::new();

        assert_eq!(q.peek_time(), None);

        q.schedule_at(500, 10, "", Box::new(|_| {}));
        q.schedule_at(300, 10, "", Box::new(|_| {}));
        q.schedule_at(400, 10, "", Box::new(|_| {}));

        assert_eq!(q.peek_time(), Some(300));
    }

    #[test]
    fn test_deterministic_ordering_large() {
        // Insert 1,000 events with random priorities, verify stable sort order.
        let mut q = EventQueue::new();

        // Fixed "random" seed – same order every time.
        let mut state: u64 = 12345;
        for _ in 0..1000 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let priority = ((state >> 32) & 0x0F) as u16; // 0-15
            let at = state % 10000;
            q.schedule_at(at, priority, "", Box::new(|_| {}));
        }

        let mut last_key: Option<QueueKey> = None;
        while let Some(event) = q.pop_next() {
            let key = event.key.unwrap();
            if let Some(last) = last_key {
                // Must be >= in ordering (Reverse means "earlier")
                assert!(last <= key, "out of order: {:?} then {:?}", last, key);
            }
            last_key = Some(key);
        }

        assert!(last_key.is_some());
    }
}
