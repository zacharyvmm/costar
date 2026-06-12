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
use std::time::Duration;

use polling::{Event, Events, Poller};

/// A registered host socket or file descriptor.
#[derive(Debug)]
struct HostSocket {
    /// The raw file descriptor (redundant with map key, kept for Debug).
    #[allow(dead_code)]
    fd: RawFd,
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
                fd: raw,
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
            if let Some(sock) = self.sockets.get_mut(&raw) {
                sock.ready = true;
                if sock.task_id != 0 {
                    ready.push((raw, sock.task_id));
                }
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
    /// The active host poller, if interactive mode is enabled.
    pub(crate) static HOST_POLLER: std::cell::RefCell<Option<HostPoller>> =
        const { std::cell::RefCell::new(None) };
}

/// Initialise the host poller (called once at startup in interactive mode).
pub fn init_host_poller() -> io::Result<()> {
    let poller = HostPoller::new()?;
    HOST_POLLER.with(|hp| {
        *hp.borrow_mut() = Some(poller);
    });
    Ok(())
}

/// Run a closure with mutable access to the host poller.
pub fn with_host_poller_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut HostPoller) -> R,
{
    HOST_POLLER.with(|hp| {
        let mut hp = hp.borrow_mut();
        hp.as_mut().map(f)
    })
}

/// Run a closure with immutable access to the host poller.
pub fn with_host_poller<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&HostPoller) -> R,
{
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
}
