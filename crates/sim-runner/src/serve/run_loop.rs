//! Cooperative JSON-RPC simulation run loop.
//!
//! Long runs execute in bounded tick batches. Between batches the owner checks
//! stop / cancel flags so a sibling TCP connection can stop or inspect the
//! session while another connection's run is active.

use std::io;
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};

use sim_world::{drive_world, RunLimit, RunTermination, SessionState, World};

/// Default virtual ticks advanced per cooperative batch.
pub const DEFAULT_TICK_BATCH: u64 = 1_000;

/// Probe whether the requesting transport connection is still alive.
///
/// Used by long-running handlers (`sim.run`, `trace.stream`) to observe client
/// disconnect between cooperative batches. Socket transports implement a
/// non-consuming peek; stdio reports always-connected because the synchronous
/// reader/worker architecture cannot detect mid-request EOF.
pub trait ConnectionLiveness {
    fn is_connected(&mut self) -> bool;
}

/// Stdio / unit-test stub: no mid-request disconnect detection.
pub struct AlwaysConnected;

impl ConnectionLiveness for AlwaysConnected {
    fn is_connected(&mut self) -> bool {
        true
    }
}

/// TCP liveness probe via `MSG_PEEK | MSG_DONTWAIT` on a cloned stream.
///
/// Does **not** put the shared socket into nonblocking mode (TCP clones share
/// one file description; flipping O_NONBLOCK would break the blocking reader).
///
/// Interprets:
/// - `0` → disconnected (EOF)
/// - `> 0` → connected (pipelined bytes left unread)
/// - `EAGAIN` / `EWOULDBLOCK` → connected, no pending input
/// - reset / broken-pipe / not-connected → disconnected
/// - other errors → conservatively treated as still connected
pub struct TcpLiveness {
    stream: TcpStream,
}

impl TcpLiveness {
    /// Build a liveness probe from a cloned TCP stream.
    pub fn from_stream(stream: TcpStream) -> io::Result<Self> {
        Ok(Self { stream })
    }
}

impl ConnectionLiveness for TcpLiveness {
    fn is_connected(&mut self) -> bool {
        let mut buf = [0u8; 1];
        let fd = self.stream.as_raw_fd();
        // Safety: fd is a live TCP socket owned by `self.stream`; recv with
        // MSG_PEEK does not consume data and MSG_DONTWAIT avoids blocking.
        let ret = unsafe {
            libc::recv(
                fd,
                buf.as_mut_ptr().cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if ret == 0 {
            return false;
        }
        if ret > 0 {
            return true;
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(code)
                if code == libc::EAGAIN || code == libc::EWOULDBLOCK || code == libc::EINTR =>
            {
                true
            }
            Some(code)
                if code == libc::ECONNRESET
                    || code == libc::EPIPE
                    || code == libc::ENOTCONN
                    || code == libc::ECONNABORTED =>
            {
                false
            }
            _ => true,
        }
    }
}

/// Shared control flags for an in-flight cooperative run.
#[derive(Debug, Default)]
pub struct RunControl {
    stop: AtomicBool,
    cancel: AtomicBool,
}

impl RunControl {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    pub fn cancel_requested(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

/// Outcome of a cooperative drive to completion (or stop/cancel).
pub struct CooperativeOutcome {
    pub state: SessionState,
    pub error: Option<String>,
    #[allow(dead_code)]
    pub events: u64,
}

/// Drive `world` toward completion in `tick_batch`-sized slices, honouring
/// `control` between batches.
///
/// Terminal states:
/// - natural idle / completion → [`SessionState::Done`]
/// - explicit stop → [`SessionState::Done`] (after `world.stop()`)
/// - cancel / disconnect → [`SessionState::Paused`]
/// - error / panic → [`SessionState::Error`]
pub fn drive_cooperative(
    world: &mut World,
    control: &RunControl,
    tick_batch: u64,
    mut on_batch: impl FnMut(&mut World) -> bool,
) -> CooperativeOutcome {
    let tick_batch = tick_batch.max(1);
    let mut events: u64 = 0;

    // Clear a leftover pause so a resumed session can advance.
    if world.is_paused() {
        world.resume();
    }

    loop {
        if control.stop_requested() {
            world.stop();
            return CooperativeOutcome {
                state: SessionState::Done,
                error: None,
                events,
            };
        }
        if control.cancel_requested() {
            return CooperativeOutcome {
                state: SessionState::Paused,
                error: None,
                events,
            };
        }

        let Some(next_event) = world.next_global_event_time() else {
            return CooperativeOutcome {
                state: SessionState::Done,
                error: None,
                events,
            };
        };
        if world.all_idle() {
            return CooperativeOutcome {
                state: SessionState::Done,
                error: None,
                events,
            };
        }

        // Jump at least to the next pending event. `drive_world` refuses to
        // advance when the next event lies beyond a nominal batch deadline and
        // returns Complete with zero progress — which would otherwise spin
        // forever for sparse schedules (e.g. now=0, batch=1000, event=10000).
        let nominal_deadline = world.now.saturating_add(tick_batch);
        let deadline = nominal_deadline.max(next_event);
        let before_now = world.now;
        let outcome = drive_world(world, RunLimit::Until(deadline));
        events = events.saturating_add(outcome.events);

        match outcome.termination {
            RunTermination::Error | RunTermination::Panic => {
                return CooperativeOutcome {
                    state: SessionState::Error,
                    error: Some(
                        outcome
                            .error
                            .unwrap_or_else(|| "simulation error".to_string()),
                    ),
                    events,
                };
            }
            RunTermination::Stopped => {
                return CooperativeOutcome {
                    state: SessionState::Done,
                    error: None,
                    events,
                };
            }
            RunTermination::Paused => {
                return CooperativeOutcome {
                    state: SessionState::Paused,
                    error: None,
                    events,
                };
            }
            RunTermination::Complete | RunTermination::LimitReached => {
                let made_progress = outcome.events > 0 || world.now > before_now;
                if !made_progress {
                    // Defensive: a pending event existed but the batch made no
                    // progress. Surface as Error instead of spinning forever.
                    return CooperativeOutcome {
                        state: SessionState::Error,
                        error: Some(format!(
                            "cooperative run made no progress with pending event at {next_event} \
                             (now={before_now}, deadline={deadline})"
                        )),
                        events,
                    };
                }
            }
        }

        // Re-check control immediately after each batch so stop/cancel from a
        // sibling connection is observed without waiting for another slice.
        if control.stop_requested() {
            world.stop();
            return CooperativeOutcome {
                state: SessionState::Done,
                error: None,
                events,
            };
        }
        if control.cancel_requested() {
            return CooperativeOutcome {
                state: SessionState::Paused,
                error: None,
                events,
            };
        }

        // Allow the transport to stream incremental output / detect disconnect.
        if !on_batch(world) {
            control.request_cancel();
            return CooperativeOutcome {
                state: SessionState::Paused,
                error: None,
                events,
            };
        }

        // Yield so sibling TCP connection threads can run stop/status handlers
        // on single-core hosts without waiting for the full simulation.
        std::thread::yield_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_world::machine::Machine;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    #[test]
    fn drive_cooperative_processes_event_beyond_nominal_batch() {
        let mut world = World::new();
        let mut machine = Machine::with_defaults(0, "sparse");
        let fired = Arc::new(AtomicU64::new(0));
        let fired_cb = Arc::clone(&fired);
        machine.schedule_at(
            10_000,
            0,
            "sparse_event",
            Box::new(move |_| {
                fired_cb.fetch_add(1, Ordering::SeqCst);
            }),
        );
        world.add_machine(machine);

        let control = RunControl::new();
        let mut batch_timestamps = Vec::new();
        let outcome = drive_cooperative(&mut world, &control, 1_000, |w| {
            batch_timestamps.push(w.now);
            // Guard against a pathological spin: refuse after many identical ticks.
            if batch_timestamps.len() > 32 {
                let all_same = batch_timestamps.windows(2).all(|w| w[0] == w[1]);
                if all_same {
                    return false;
                }
            }
            true
        });

        assert_eq!(outcome.state, SessionState::Done);
        assert!(
            outcome.error.is_none(),
            "unexpected error: {:?}",
            outcome.error
        );
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "sparse event must fire once"
        );
        assert!(
            world.now >= 10_000,
            "world.now must reach the event time, got {}",
            world.now
        );
        assert!(
            !batch_timestamps.is_empty(),
            "on_batch should observe at least one batch"
        );
        // Timestamps must be monotonically non-decreasing and not an unbounded
        // sequence of identical values at the pre-event clock.
        for window in batch_timestamps.windows(2) {
            assert!(
                window[1] >= window[0],
                "batch timestamps must be non-decreasing: {batch_timestamps:?}"
            );
        }
        let stagnant = batch_timestamps.iter().filter(|&&t| t == 0).count();
        assert!(
            stagnant <= 1,
            "must not emit unbounded unchanged timestamps at t=0: {batch_timestamps:?}"
        );
    }
}
