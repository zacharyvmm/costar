//! Thread-local active yielder.
//!
//! C port hooks (like `sim_port_yield`) need a way to suspend the currently
//! active coroutine.  We store a pointer to the active `Yielder` in TLS
//! only during the window when a coroutine is being resumed.
//!
//! # Safety invariants
//!
//! 1. Set `ACTIVE_YIELDER` immediately before entering/resuming a coroutine.
//! 2. Clear it immediately when returning to the parent stack.
//! 3. Never store the pointer beyond the active resume window.
//! 4. Never call `suspend` if no yielder is active.
//! 5. Assert single-threaded execution in debug builds.

use std::cell::Cell;
use std::ptr::NonNull;

use corosensei::Yielder;

use crate::yield_reason::{ResumeReason, YieldReason};

/// Type alias for the yielder used by the fiber runtime.
pub type SimYielder = Yielder<ResumeReason, YieldReason>;

thread_local! {
    /// The currently active yielder, if any.
    pub(crate) static ACTIVE_YIELDER: Cell<Option<NonNull<SimYielder>>> =
        const { Cell::new(None) };
}

/// Set the active yielder pointer.
///
/// # Safety
///
/// The pointer must be valid for the duration of the coroutine resume.
pub(crate) fn set_active_yielder(yielder: &SimYielder) {
    let ptr = NonNull::from(yielder);
    ACTIVE_YIELDER.with(|cell| {
        cell.set(Some(ptr));
    });
}

/// Suspend the currently active fiber from outside the coroutine
/// (e.g., from a C FFI callback like `sim_port_yield`).
///
/// Returns `true` if a fiber was active and suspended.
/// Returns `false` if no fiber was active (caller error).
pub fn suspend_active_fiber(reason: YieldReason) -> bool {
    ACTIVE_YIELDER.with(|cell| {
        if let Some(ptr) = cell.get() {
            // Safety: the pointer is valid because we only set it during
            // an active coroutine resume window and clear it before returning.
            let yielder = unsafe { ptr.as_ref() };
            yielder.suspend(reason);
            true
        } else {
            false
        }
    })
}

/// Clear the active yielder without checking (used by the scheduler
/// after a task yields).
pub fn clear_active_yielder_for_scheduler() {
    ACTIVE_YIELDER.with(|cell| {
        cell.set(None);
    });
}

/// Whether a fiber is currently active.
pub fn has_active_fiber() -> bool {
    ACTIVE_YIELDER.with(|cell| cell.get().is_some())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_active_fiber_initially() {
        assert!(!has_active_fiber());
    }

    #[test]
    fn test_suspend_without_fiber_returns_false() {
        assert!(!suspend_active_fiber(YieldReason::Cooperative));
    }
}
