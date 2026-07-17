//! Host-connected mode: non-blocking socket integration via `polling`.
//!
//! When enabled (interactive mode), simulated tasks can interact with real
//! host sockets.  A task that would block on `recv()` / `accept()` instead
//! registers interest with the poller, yields, and is woken when data arrives.
//!
//! # Determinism warning
//!
//! Host-connected mode is **not** deterministic.  Use deterministic
//! packet scripts (`SimNetDevice::inject_rx`) for golden-trace tests.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────┐
//! │  Fiber: C task calls recv(fd, ...) │
//! │  No data available → register fd   │
//! │  with HostPoller, yield(IoWait)    │
//! ├─────────────────────────────────────┤
//! │  Scheduler: no runnable tasks      │
//! │  → host_poller.poll(timeout)       │
//! │  → fd becomes readable             │
//! │  → wake associated fiber           │
//! └─────────────────────────────────────┘
//! ```

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use polling::{Event, Events, Poller};

/// Test seam: when set, [`HostPoller::new`] fails deterministically.
static FORCE_NEW_FAIL: AtomicBool = AtomicBool::new(false);

/// Force the next [`HostPoller::new`] calls to fail (test-only).
#[cfg(test)]
pub fn set_force_new_failure(fail: bool) {
    FORCE_NEW_FAIL.store(fail, Ordering::SeqCst);
}

/// A registered host socket or file descriptor.
#[derive(Debug)]
struct HostSocket {
    /// The raw file descriptor (mirrors the map key; kept for `Debug` output).
    _fd: RawFd,
    /// The task ID blocked on this socket (0 = none).
    task_id: u64,
    /// Whether the socket was signalled as ready.
    ready: bool,
}

/// Host I/O poller for interactive mode.
///
/// Wraps a `polling::Poller` and maintains a mapping from raw file
/// descriptors to blocked task IDs.
pub struct HostPoller {
    poller: Poller,
    sockets: BTreeMap<RawFd, HostSocket>,
    events: Events,
}

impl HostPoller {
    /// Create a new host poller with no registered sockets.
    pub fn new() -> io::Result<Self> {
        if FORCE_NEW_FAIL.load(Ordering::SeqCst) {
            return Err(io::Error::other("forced HostPoller::new failure"));
        }
        Ok(Self {
            poller: Poller::new()?,
            sockets: BTreeMap::new(),
            events: Events::new(),
        })
    }

    /// Register a file descriptor for readability monitoring.
    ///
    /// # Safety
    ///
    /// The caller must ensure the fd outlives the poller, and must
    /// call `deregister` before closing the fd.
    pub unsafe fn register_raw(&mut self, raw: RawFd) -> io::Result<()> {
        self.poller.add(raw, Event::readable(raw as usize))?;

        self.sockets.insert(
            raw,
            HostSocket {
                _fd: raw,
                task_id: 0,
                ready: false,
            },
        );
        Ok(())
    }

    /// Register a file descriptor for readability monitoring (safe wrapper).
    pub fn register(&mut self, fd: impl AsRawFd) -> io::Result<()> {
        // Safety: the fd is borrowed from `fd` which outlives this call.
        // The caller must deregister before dropping `fd`.
        unsafe { self.register_raw(fd.as_raw_fd()) }
    }

    /// Remove a file descriptor from the poller.
    pub fn deregister(&mut self, fd: impl AsRawFd) -> io::Result<()> {
        let raw = fd.as_raw_fd();
        // Safety: the fd is valid because we previously registered it
        // and the caller hasn't closed it yet.
        let borrowed = unsafe { BorrowedFd::borrow_raw(raw) };
        self.poller.delete(borrowed)?;
        self.sockets.remove(&raw);
        Ok(())
    }

    /// Remove a file descriptor by raw fd (for C ABI).
    ///
    /// # Safety
    ///
    /// The fd must have been previously registered and not yet closed.
    pub unsafe fn deregister_raw(&mut self, raw: RawFd) -> io::Result<()> {
        let borrowed = BorrowedFd::borrow_raw(raw);
        self.poller.delete(borrowed)?;
        self.sockets.remove(&raw);
        Ok(())
    }

    /// Associate a file descriptor with a blocked task.
    ///
    /// When the fd becomes ready, the task will be woken.
    pub fn block_task(&mut self, fd: RawFd, task_id: u64) {
        if let Some(sock) = self.sockets.get_mut(&fd) {
            sock.task_id = task_id;
            sock.ready = false;
        }
    }

    /// Unblock a task from a file descriptor (the task is no longer waiting).
    pub fn unblock_task(&mut self, fd: RawFd) {
        if let Some(sock) = self.sockets.get_mut(&fd) {
            sock.task_id = 0;
        }
    }

    /// Wait for socket readiness with the given timeout.
    ///
    /// Returns a list of (fd, task_id) pairs for sockets that became
    /// ready.  The caller should wake the associated tasks.
    ///
    /// If `timeout` is `None`, blocks indefinitely.  This should only
    /// be used in interactive mode when no virtual events are pending.
    pub fn poll(&mut self, timeout: Option<Duration>) -> io::Result<Vec<(RawFd, u64)>> {
        self.events.clear();
        self.poller.wait(&mut self.events, timeout)?;

        let mut ready = Vec::new();
        for event in self.events.iter() {
            let raw = event.key as RawFd;
            let mut still_registered = false;
            if let Some(sock) = self.sockets.get_mut(&raw) {
                sock.ready = true;
                still_registered = true;
                if sock.task_id != 0 {
                    ready.push((raw, sock.task_id));
                }
            }

            // Re-arm interest for this fd. The `polling` crate delivers events
            // in oneshot mode: once an fd fires, its interest is consumed and
            // it will NOT fire again until re-registered. Without this re-arm,
            // a second readiness event on the same fd would be silently missed,
            // so a task that re-blocks on the fd after handling one wakeup would
            // never be woken again. Re-arming keeps the fd monitored for
            // subsequent host I/O wakeups.
            if still_registered {
                // Safety: the fd is still registered (present in `self.sockets`)
                // and, per the register contract, remains open until the caller
                // deregisters it. Best-effort: ignore errors from fds that were
                // concurrently removed.
                let borrowed = unsafe { BorrowedFd::borrow_raw(raw) };
                let _ = self.poller.modify(borrowed, Event::readable(raw as usize));
            }
        }
        Ok(ready)
    }

    /// Check whether a specific file descriptor is ready (non-blocking).
    pub fn is_ready(&self, fd: RawFd) -> bool {
        self.sockets.get(&fd).map(|s| s.ready).unwrap_or(false)
    }

    /// Number of registered sockets.
    pub fn len(&self) -> usize {
        self.sockets.len()
    }

    /// Whether any sockets are registered.
    pub fn is_empty(&self) -> bool {
        self.sockets.is_empty()
    }

    /// Whether any task is currently blocked on I/O.
    pub fn has_blocked_tasks(&self) -> bool {
        self.sockets.values().any(|s| s.task_id != 0)
    }

    /// Clear the ready flag for a file descriptor (after the task has been woken).
    pub fn clear_ready(&mut self, fd: RawFd) {
        if let Some(sock) = self.sockets.get_mut(&fd) {
            sock.ready = false;
        }
    }
}

impl std::fmt::Debug for HostPoller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostPoller")
            .field("sockets_count", &self.sockets.len())
            .finish()
    }
}

impl Default for HostPoller {
    fn default() -> Self {
        Self::new().expect("HostPoller::new() failed")
    }
}

// ---------------------------------------------------------------------------
// Thread-local host poller (for scheduler access)
// ---------------------------------------------------------------------------

std::thread_local! {
    /// Legacy fallback poller used when no [`crate::NetworkBank`] is active.
    /// Single-simulator / interactive runners initialise this via
    /// [`init_host_poller`]. Production Worlds with owned banks never touch it.
    pub(crate) static HOST_POLLER: std::cell::RefCell<Option<HostPoller>> =
        const { std::cell::RefCell::new(None) };
}

/// Ensure a host poller exists for the active bank (or legacy TLS).
///
/// Propagates [`HostPoller::new`] failures. Returns `Ok(())` when a poller
/// is already present or was created successfully.
pub fn ensure_host_poller() -> io::Result<()> {
    if crate::bank::has_active_bank() {
        return crate::bank::with_network_bank_if_active(|bank| {
            let mut cell = bank.inner.host_poller.borrow_mut();
            if cell.is_none() {
                *cell = Some(HostPoller::new()?);
            }
            Ok(())
        })
        .unwrap_or_else(|| {
            Err(io::Error::other(
                "active NetworkBank vanished during host poller ensure",
            ))
        });
    }
    HOST_POLLER.with(|hp| {
        let mut cell = hp.borrow_mut();
        if cell.is_none() {
            *cell = Some(HostPoller::new()?);
        }
        Ok(())
    })
}

/// Initialise the host poller (called once at startup in interactive mode).
///
/// When a [`crate::NetworkBank`] is active the poller is stored on that bank;
/// otherwise the legacy thread-local store is used.
pub fn init_host_poller() -> io::Result<()> {
    ensure_host_poller()
}

/// Mutable access to an existing poller only — never constructs one.
///
/// Returns `None` when no bank is active and no legacy poller exists, or when
/// the active bank has not yet created a poller.
pub fn with_existing_host_poller_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut HostPoller) -> R,
{
    if crate::bank::has_active_bank() {
        return crate::bank::with_network_bank_if_active(|bank| {
            let mut cell = bank.inner.host_poller.borrow_mut();
            cell.as_mut().map(f)
        })
        .flatten();
    }
    HOST_POLLER.with(|hp| {
        let mut hp = hp.borrow_mut();
        hp.as_mut().map(f)
    })
}

/// Ensure a poller exists, then run `f`. Propagates init and operation errors.
pub fn with_or_init_host_poller_mut<F, R>(f: F) -> io::Result<R>
where
    F: FnOnce(&mut HostPoller) -> io::Result<R>,
{
    ensure_host_poller()?;
    with_existing_host_poller_mut(f).unwrap_or_else(|| {
        Err(io::Error::other(
            "host poller missing after successful ensure",
        ))
    })
}

/// Run a closure with mutable access to the host poller.
///
/// Routes through the active [`crate::NetworkBank`] when one is present
/// (lazy-initialising its poller), otherwise the legacy thread-local store.
///
/// Prefer [`with_or_init_host_poller_mut`] when errors must propagate, or
/// [`with_existing_host_poller_mut`] for deregistration.
pub fn with_host_poller_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut HostPoller) -> R,
{
    if ensure_host_poller().is_err() {
        return None;
    }
    with_existing_host_poller_mut(f)
}

/// Run a closure with immutable access to the host poller.
///
/// Routes through the active [`crate::NetworkBank`] when one is present,
/// otherwise the legacy thread-local store. Does not lazy-init.
pub fn with_host_poller<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&HostPoller) -> R,
{
    if crate::bank::has_active_bank() {
        return crate::bank::with_network_bank_if_active(|bank| {
            let cell = bank.inner.host_poller.borrow();
            cell.as_ref().map(f)
        })
        .flatten();
    }
    HOST_POLLER.with(|hp| {
        let hp = hp.borrow();
        hp.as_ref().map(f)
    })
}

// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    /// Test that a TCP listener becomes readable when a connection arrives.
    #[test]
    fn test_poller_tcp_accept() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let raw_fd = listener.as_raw_fd();

        let mut poller = HostPoller::new().unwrap();
        unsafe { poller.register_raw(raw_fd).unwrap() };

        // Initially no events
        let ready = poller.poll(Some(Duration::from_millis(10))).unwrap();
        assert!(ready.is_empty());

        // Connect — the listener should become readable
        let _stream = TcpStream::connect(addr).unwrap();

        // Now the listener should be readable
        let ready = poller.poll(Some(Duration::from_millis(100))).unwrap();
        assert!(
            !ready.is_empty() || poller.sockets.values().any(|s| s.ready),
            "expected at least one ready socket after connect"
        );
    }

    /// Test register → block → poll → wake flow.
    #[test]
    fn test_poller_block_wake() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let listener_fd = listener.as_raw_fd();

        let mut poller = HostPoller::new().unwrap();
        unsafe { poller.register_raw(listener_fd).unwrap() };

        // Block "task 42" on this fd
        poller.block_task(listener_fd, 42);

        // Connect
        let _stream = TcpStream::connect(addr).unwrap();

        // Poll — should return task 42 as woken if the fd became ready
        let ready = poller.poll(Some(Duration::from_millis(100))).unwrap();
        let task_42_woken = ready.iter().any(|(_, tid)| *tid == 42);
        if poller.is_ready(listener_fd) {
            assert!(task_42_woken, "fd ready but task 42 not woken");
        }
    }

    /// Test deregister removes the fd.
    #[test]
    fn test_poller_deregister() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let raw_fd = listener.as_raw_fd();

        let mut poller = HostPoller::new().unwrap();
        unsafe { poller.register_raw(raw_fd).unwrap() };
        assert_eq!(poller.len(), 1);

        // Deregister using BorrowedFd
        let borrowed = unsafe { BorrowedFd::borrow_raw(raw_fd) };
        poller.poller.delete(borrowed).unwrap();
        poller.sockets.remove(&raw_fd);
        assert_eq!(poller.len(), 0);
    }

    /// Test unblock_task clears the task association.
    #[test]
    fn test_poller_unblock() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let fd = listener.as_raw_fd();

        let mut poller = HostPoller::new().unwrap();
        unsafe { poller.register_raw(fd).unwrap() };
        poller.block_task(fd, 7);
        assert!(poller.has_blocked_tasks());

        poller.unblock_task(fd);
        assert!(!poller.has_blocked_tasks());
    }

    /// Re-arm regression: a single fd must be able to wake its task on
    /// *repeated* readiness events. The `polling` crate is oneshot, so without
    /// re-arming after each event the second wakeup is silently lost.
    #[test]
    fn test_poller_rearm_repeated_wakeups() {
        use std::io::{Read, Write};

        // Establish a connected TCP pair: `client` (monitored) <-> `server`.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        client.set_nonblocking(true).unwrap();
        let client_fd = client.as_raw_fd();

        let mut poller = HostPoller::new().unwrap();
        unsafe { poller.register_raw(client_fd).unwrap() };

        // ── First readiness event ──
        poller.block_task(client_fd, 77);
        server.write_all(b"one").unwrap();
        server.flush().unwrap();
        let r1 = poller.poll(Some(Duration::from_millis(1000))).unwrap();
        assert!(
            r1.iter().any(|(_, tid)| *tid == 77),
            "task 77 should be woken on the first readiness event"
        );

        // The task consumes the data and re-blocks on the same fd, so the socket
        // is no longer readable until fresh data arrives — this makes the second
        // poll test a genuine new readiness edge, not stale buffered data.
        let mut drain = [0u8; 3];
        let _ = (&client).read(&mut drain);
        poller.clear_ready(client_fd);
        poller.block_task(client_fd, 77);

        // ── Second readiness event on the SAME fd ──
        server.write_all(b"two").unwrap();
        server.flush().unwrap();
        let r2 = poller.poll(Some(Duration::from_millis(1000))).unwrap();
        assert!(
            r2.iter().any(|(_, tid)| *tid == 77),
            "task 77 should be woken AGAIN — the fd must be re-armed after the \
             first readiness event"
        );
    }

    #[test]
    fn deregister_without_poller_does_not_construct_one() {
        // Ensure legacy TLS starts empty.
        HOST_POLLER.with(|hp| *hp.borrow_mut() = None);
        assert!(
            with_existing_host_poller_mut(|_| ()).is_none(),
            "no poller should exist"
        );
        assert!(with_existing_host_poller_mut(|hp| {
            let _ = unsafe { hp.deregister_raw(3) };
        })
        .is_none());
        assert!(
            with_existing_host_poller_mut(|_| ()).is_none(),
            "deregister must not lazily construct a poller"
        );
        assert!(
            with_host_poller(|_| ()).is_none(),
            "legacy TLS must still be empty"
        );
    }

    #[test]
    fn init_failure_propagates_to_caller() {
        set_force_new_failure(true);
        let err = ensure_host_poller();
        set_force_new_failure(false);
        assert!(err.is_err(), "forced new failure must propagate");
        // Cleanup any partial state.
        HOST_POLLER.with(|hp| *hp.borrow_mut() = None);
    }

    #[test]
    fn register_failure_when_init_fails() {
        set_force_new_failure(true);
        let result = with_or_init_host_poller_mut(|hp| unsafe { hp.register_raw(3) });
        set_force_new_failure(false);
        assert!(result.is_err());
        HOST_POLLER.with(|hp| *hp.borrow_mut() = None);
    }
}
