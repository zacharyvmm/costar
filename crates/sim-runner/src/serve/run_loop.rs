//! Cooperative JSON-RPC simulation run loop.
//!
//! Long runs execute in bounded tick batches. Between batches the owner checks
//! stop / cancel flags so a sibling TCP connection can stop or inspect the
//! session while another connection's run is active.

use std::io;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};

use sim_world::{drive_world, RunLimit, RunTermination, SessionState, World};

/// Default virtual ticks advanced per cooperative batch.
pub const DEFAULT_TICK_BATCH: u64 = 1_000;

/// Platform-specific TCP socket liveness probe.
#[cfg(unix)]
fn platform_socket_is_connected(stream: &TcpStream) -> bool {
    use std::os::fd::AsRawFd;

    let mut buf = [0u8; 1];
    let fd = stream.as_raw_fd();
    // Safety: fd is a live TCP socket owned by `stream`; recv with
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
        Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK || code == libc::EINTR => {
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

/// Platform-specific TCP socket liveness probe (Windows).
///
/// Uses `WSAPoll` with zero timeout to detect readability / hangup without
/// changing the shared socket's blocking mode, then `recv(MSG_PEEK)` only
/// when the socket is readable.
#[cfg(windows)]
fn platform_socket_is_connected(stream: &TcpStream) -> bool {
    use std::os::windows::io::AsRawSocket;

    use windows_sys::Win32::Networking::WinSock::{
        recv, WSAPoll, MSG_PEEK, POLLERR, POLLHUP, POLLRDNORM, SOCKET_ERROR, WSAECONNABORTED,
        WSAECONNRESET, WSAEINTR, WSAENOTCONN, WSAESHUTDOWN, WSAEWOULDBLOCK, WSAPOLLFD,
    };

    let socket = stream.as_raw_socket() as usize;
    let mut poll_fd = WSAPOLLFD {
        fd: socket,
        events: POLLRDNORM | POLLERR | POLLHUP,
        revents: 0,
    };

    // Safety: `poll_fd` points at a valid socket owned by `stream`.
    let poll_ret = unsafe { WSAPoll(&mut poll_fd, 1, 0) };
    if poll_ret == SOCKET_ERROR {
        let err = io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(code) if code == WSAEINTR => true,
            _ => true,
        };
    }

    let revents = poll_fd.revents;
    if revents & (POLLERR | POLLHUP) != 0 {
        return false;
    }
    if revents & POLLRDNORM == 0 {
        // No pending input — still connected.
        return true;
    }

    let mut buf = [0u8; 1];
    // Safety: socket is readable per WSAPoll; MSG_PEEK does not consume data.
    let ret = unsafe { recv(socket, buf.as_mut_ptr().cast(), 1, MSG_PEEK) };
    if ret == 0 {
        return false;
    }
    if ret > 0 {
        return true;
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == WSAEWOULDBLOCK || code == WSAEINTR => true,
        Some(code)
            if code == WSAECONNRESET
                || code == WSAECONNABORTED
                || code == WSAENOTCONN
                || code == WSAESHUTDOWN =>
        {
            false
        }
        _ => true,
    }
}

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

/// TCP liveness probe via non-consuming socket peek.
///
/// Does **not** put the shared socket into nonblocking mode (TCP clones share
/// one file description / socket state; flipping O_NONBLOCK would break the
/// blocking reader).
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
        platform_socket_is_connected(&self.stream)
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
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

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

    fn connected_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        listener.set_nonblocking(true).expect("listener nb");
        let addr = listener.local_addr().expect("local addr");
        let client = TcpStream::connect(addr).expect("connect client");
        let server = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => panic!("accept failed: {e}"),
            }
        };
        client.set_nonblocking(false).expect("client blocking");
        server.set_nonblocking(false).expect("server blocking");
        (client, server)
    }

    #[test]
    fn tcp_liveness_connected_no_data_reports_alive() {
        let (client, _peer) = connected_pair();
        let probe = TcpLiveness::from_stream(client).expect("probe");
        let mut probe = probe;
        assert!(probe.is_connected());
    }

    #[test]
    fn tcp_liveness_unread_bytes_not_consumed() {
        let (mut client, mut peer) = connected_pair();
        peer.write_all(b"x").expect("write byte");
        peer.flush().expect("flush");

        let mut probe =
            TcpLiveness::from_stream(client.try_clone().expect("clone")).expect("probe");
        assert!(probe.is_connected());

        let mut buf = [0u8; 1];
        client.read_exact(&mut buf).expect("read byte");
        assert_eq!(buf, [b'x']);
    }

    #[test]
    fn tcp_liveness_peer_drop_eventually_false() {
        let (client, peer) = connected_pair();
        drop(peer);

        let mut probe = TcpLiveness::from_stream(client).expect("probe");
        let mut saw_disconnected = false;
        for _ in 0..50 {
            if !probe.is_connected() {
                saw_disconnected = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            saw_disconnected,
            "peer drop must eventually read as disconnected"
        );
    }

    #[test]
    fn tcp_liveness_repeated_probes_do_not_break_blocking_reads() {
        let (mut client, mut peer) = connected_pair();
        peer.write_all(b"hello").expect("write payload");
        peer.flush().expect("flush");

        let mut probe =
            TcpLiveness::from_stream(client.try_clone().expect("clone")).expect("probe");
        for _ in 0..8 {
            assert!(probe.is_connected());
        }

        let mut buf = [0u8; 5];
        client
            .read_exact(&mut buf)
            .expect("blocking read after probes");
        assert_eq!(&buf, b"hello");
    }
}
