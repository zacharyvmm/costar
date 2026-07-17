//! Cooperative batch driving shared by JSON-RPC and gRPC run loops.
//!
//! Long-running simulations advance in bounded virtual-time slices. Unbounded
//! runs must jump to sparse pending events instead of spinning with zero
//! progress; bounded runs must pause at an absolute deadline without executing
//! events beyond it.

use crate::control::{drive_world, RunLimit, RunOutcome, RunTermination};
use crate::world::World;

/// Outcome of one cooperative batch slice.
pub enum CooperativeBatchOutcome {
    /// [`drive_world`] ran for this batch (check [`RunOutcome::termination`]).
    Driven(RunOutcome),
    /// Advanced virtual time to the absolute deadline without executing
    /// past-deadline events.
    PausedAtDeadline {
        /// Virtual time after pausing.
        now: u64,
    },
    /// No pending work / all machines idle — caller should treat as Done.
    Idle,
    /// Zero progress with a pending event — caller should surface as Error.
    NoProgress {
        /// Virtual time before the batch.
        before_now: u64,
        /// Earliest pending global event.
        next_event: u64,
        /// Batch virtual deadline that was requested.
        batch_deadline: u64,
        /// Absolute run deadline, if any.
        absolute_deadline: Option<u64>,
    },
}

impl std::fmt::Debug for CooperativeBatchOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Driven(outcome) => f
                .debug_struct("Driven")
                .field("termination", &outcome.termination)
                .field("now", &outcome.now)
                .field("events", &outcome.events)
                .finish(),
            Self::PausedAtDeadline { now } => f
                .debug_struct("PausedAtDeadline")
                .field("now", now)
                .finish(),
            Self::Idle => write!(f, "Idle"),
            Self::NoProgress {
                before_now,
                next_event,
                batch_deadline,
                absolute_deadline,
            } => f
                .debug_struct("NoProgress")
                .field("before_now", before_now)
                .field("next_event", next_event)
                .field("batch_deadline", batch_deadline)
                .field("absolute_deadline", absolute_deadline)
                .finish(),
        }
    }
}

impl CooperativeBatchOutcome {
    /// Human-readable error for [`CooperativeBatchOutcome::NoProgress`].
    pub fn no_progress_message(&self) -> Option<String> {
        match self {
            Self::NoProgress {
                before_now,
                next_event,
                batch_deadline,
                absolute_deadline,
            } => Some(format!(
                "cooperative run made no progress with pending event at {next_event} \
                 (now={before_now}, batch_deadline={batch_deadline}, \
                 absolute_deadline={absolute_deadline:?})"
            )),
            _ => None,
        }
    }
}

/// Compute the virtual deadline for one cooperative batch.
///
/// Returns `Ok(None)` when there is no pending work ([`CooperativeBatchOutcome::Idle`]).
/// Returns `Ok(Some(deadline))` when a batch should call [`drive_world`] with
/// [`RunLimit::Until`]. Returns `Err` when the absolute deadline is already
/// reached or the next event lies strictly beyond it (caller should pause at
/// the absolute deadline without executing that event).
pub fn cooperative_batch_deadline(
    world: &World,
    tick_batch: u64,
    absolute_deadline: Option<u64>,
) -> Result<Option<u64>, u64> {
    let tick_batch = tick_batch.max(1);

    let Some(next_event) = world.next_global_event_time() else {
        return Ok(None);
    };
    if world.all_idle() {
        return Ok(None);
    }

    match absolute_deadline {
        None => {
            let nominal = world.now.saturating_add(tick_batch);
            Ok(Some(nominal.max(next_event)))
        }
        Some(d) => {
            if world.now >= d {
                return Err(d);
            }
            if next_event > d {
                return Err(d);
            }
            let nominal = world.now.saturating_add(tick_batch);
            Ok(Some(nominal.max(next_event).min(d)))
        }
    }
}

/// Drive one cooperative batch slice.
pub fn drive_cooperative_batch(
    world: &mut World,
    tick_batch: u64,
    absolute_deadline: Option<u64>,
) -> CooperativeBatchOutcome {
    let tick_batch = tick_batch.max(1);

    let Some(next_event) = world.next_global_event_time() else {
        return CooperativeBatchOutcome::Idle;
    };
    if world.all_idle() {
        return CooperativeBatchOutcome::Idle;
    }

    match absolute_deadline {
        None => drive_unbounded_batch(world, tick_batch, next_event),
        Some(d) => drive_bounded_batch(world, tick_batch, next_event, d),
    }
}

fn drive_unbounded_batch(
    world: &mut World,
    tick_batch: u64,
    next_event: u64,
) -> CooperativeBatchOutcome {
    let before_now = world.now;
    let batch_deadline = world.now.saturating_add(tick_batch).max(next_event);
    let outcome = drive_world(world, RunLimit::Until(batch_deadline));

    if is_terminal_drive(&outcome.termination) {
        return CooperativeBatchOutcome::Driven(outcome);
    }

    let made_progress = outcome.events > 0 || world.now > before_now;
    if !made_progress {
        return CooperativeBatchOutcome::NoProgress {
            before_now,
            next_event,
            batch_deadline,
            absolute_deadline: None,
        };
    }

    CooperativeBatchOutcome::Driven(outcome)
}

fn drive_bounded_batch(
    world: &mut World,
    tick_batch: u64,
    next_event: u64,
    absolute_deadline: u64,
) -> CooperativeBatchOutcome {
    if world.now >= absolute_deadline {
        return CooperativeBatchOutcome::PausedAtDeadline { now: world.now };
    }

    if next_event > absolute_deadline {
        world.now = absolute_deadline;
        return CooperativeBatchOutcome::PausedAtDeadline {
            now: absolute_deadline,
        };
    }

    let batch_deadline = world
        .now
        .saturating_add(tick_batch)
        .max(next_event)
        .min(absolute_deadline);
    let outcome = drive_world(world, RunLimit::Until(batch_deadline));

    if is_terminal_drive(&outcome.termination) {
        return CooperativeBatchOutcome::Driven(outcome);
    }

    // Advance across empty virtual time up to the batch boundary when the next
    // event is strictly after that boundary (matching gRPC handoff semantics).
    if world.now < batch_deadline
        && world
            .next_global_event_time()
            .is_some_and(|t| t > batch_deadline)
        && !world.all_idle()
    {
        world.now = batch_deadline;
    }

    if world.now >= absolute_deadline {
        return CooperativeBatchOutcome::PausedAtDeadline { now: world.now };
    }

    CooperativeBatchOutcome::Driven(outcome)
}

fn is_terminal_drive(termination: &RunTermination) -> bool {
    matches!(
        termination,
        RunTermination::Error
            | RunTermination::Panic
            | RunTermination::Paused
            | RunTermination::Stopped
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::Machine;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    fn sparse_world(event_at: u64) -> (World, Arc<AtomicU64>) {
        let mut world = World::new();
        let mut machine = Machine::with_defaults(0, "sparse");
        let fired = Arc::new(AtomicU64::new(0));
        let fired_cb = Arc::clone(&fired);
        machine.schedule_at(
            event_at,
            0,
            "sparse_event",
            Box::new(move |_| {
                fired_cb.fetch_add(1, Ordering::SeqCst);
            }),
        );
        world.add_machine(machine);
        (world, fired)
    }

    #[test]
    fn cooperative_batch_deadline_unbounded_jumps_to_sparse_event() {
        let (world, _) = sparse_world(10_000);
        let deadline = cooperative_batch_deadline(&world, 1_000, None).unwrap();
        assert_eq!(deadline, Some(10_000));
    }

    #[test]
    fn cooperative_batch_deadline_bounded_within_absolute() {
        let (mut world, _) = sparse_world(3_000);
        let deadline = cooperative_batch_deadline(&world, 1_000, Some(5_000)).unwrap();
        assert_eq!(deadline, Some(3_000));

        let outcome = drive_cooperative_batch(&mut world, 1_000, Some(5_000));
        assert!(matches!(outcome, CooperativeBatchOutcome::Driven(_)));
        assert_eq!(world.now, 3_000);
    }

    #[test]
    fn cooperative_batch_deadline_bounded_next_beyond_absolute_err() {
        let (world, _) = sparse_world(10_000);
        assert_eq!(
            cooperative_batch_deadline(&world, 1_000, Some(5_000)),
            Err(5_000)
        );
    }

    #[test]
    fn unbounded_sparse_event_fires_once() {
        let (mut world, fired) = sparse_world(10_000);
        let outcome = drive_cooperative_batch(&mut world, 1_000, None);
        assert!(matches!(outcome, CooperativeBatchOutcome::Driven(_)));
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert!(world.now >= 10_000);
    }

    #[test]
    fn bounded_sparse_event_pauses_without_firing() {
        let (mut world, fired) = sparse_world(10_000);
        let outcome = drive_cooperative_batch(&mut world, 1_000, Some(5_000));
        assert!(matches!(
            outcome,
            CooperativeBatchOutcome::PausedAtDeadline { now: 5_000 }
        ));
        assert_eq!(fired.load(Ordering::SeqCst), 0);
        assert_eq!(world.now, 5_000);
    }

    #[test]
    fn bounded_sparse_event_resumes_and_fires() {
        let (mut world, fired) = sparse_world(10_000);
        let first = drive_cooperative_batch(&mut world, 1_000, Some(5_000));
        assert!(matches!(
            first,
            CooperativeBatchOutcome::PausedAtDeadline { now: 5_000 }
        ));

        let second = drive_cooperative_batch(&mut world, 1_000, Some(20_000));
        assert!(matches!(second, CooperativeBatchOutcome::Driven(_)));
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert!(world.now >= 10_000);
    }

    #[test]
    fn no_progress_outcome_message_includes_diagnostics() {
        let outcome = CooperativeBatchOutcome::NoProgress {
            before_now: 0,
            next_event: 10_000,
            batch_deadline: 10_000,
            absolute_deadline: None,
        };
        let msg = outcome.no_progress_message().expect("message");
        assert!(msg.contains("no progress"));
        assert!(msg.contains("10000"));
        assert!(msg.contains("batch_deadline=10000"));
    }

    #[test]
    fn no_progress_detected_when_batch_makes_no_advance() {
        // Defensive path: if progress checking were bypassed, surface diagnostics.
        let outcome = CooperativeBatchOutcome::NoProgress {
            before_now: 42,
            next_event: 99,
            batch_deadline: 99,
            absolute_deadline: Some(200),
        };
        assert_eq!(
            outcome.no_progress_message().unwrap(),
            "cooperative run made no progress with pending event at 99 \
             (now=42, batch_deadline=99, absolute_deadline=Some(200))"
        );
    }
}
