//! # sim-zephyr-port
//!
//! Rust side of the Zephyr simulator arch port.
//!
//! Provides the Zephyr-specific thread registry (mapping Zephyr TCBs to
//! Rust fiber IDs), the scheduler lock, and current-thread tracking.
//!
//! The actual scheduler loop lives in `sim-ffi/src/lib.rs` as
//! `sim_zephyr_start_scheduler()` — it's there because it needs access to
//! `SIM_GLOBAL`, `SIM_NOW`, `CURRENT_TASK_ID`, and the fiber runtime.

use std::cell::Cell;

// ---------------------------------------------------------------------------
// Zephyr thread registry
// ---------------------------------------------------------------------------

/// Maps a Zephyr thread pointer (TCB) to a Rust fiber task ID.
///
/// Used by `sim_zephyr_set_current_thread` and `sim_zephyr_get_current_thread`
/// to synchronize Zephyr's `_kernel.current` with the Rust fiber about to
/// be resumed.
#[derive(Debug, Clone, Copy, Default)]
pub struct ZephyrThreadRegistry {
    /// Maximum number of registered threads.
    entries: [Option<(usize, u64)>; 16],
    /// Number of entries currently in use.
    count: usize,
}

impl ZephyrThreadRegistry {
    pub const fn new() -> Self {
        Self {
            entries: [None; 16],
            count: 0,
        }
    }

    /// Register a TCB → fiber ID mapping.
    pub fn register(&mut self, tcb: usize, fiber_id: u64) {
        if self.count < self.entries.len() {
            self.entries[self.count] = Some((tcb, fiber_id));
            self.count += 1;
        }
    }

    /// Look up the fiber ID for a given TCB.
    pub fn fiber_id_for(&self, tcb: usize) -> Option<u64> {
        self.entries
            .iter()
            .filter_map(|e| *e)
            .find_map(|(t, fid)| if t == tcb { Some(fid) } else { None })
    }
}

thread_local! {
    /// The Zephyr thread registry.
    pub(crate) static ZEPHYR_REGISTRY: std::cell::RefCell<ZephyrThreadRegistry> =
        const { std::cell::RefCell::new(ZephyrThreadRegistry::new()) };
}

/// Register a TCB → fiber ID mapping.
pub fn zephyr_register_tcb(tcb: usize, fiber_id: u64) {
    ZEPHYR_REGISTRY.with(|r| {
        r.borrow_mut().register(tcb, fiber_id);
    });
}

/// Look up the fiber ID for a TCB.
pub fn zephyr_fiber_id_for_tcb(tcb: usize) -> Option<u64> {
    ZEPHYR_REGISTRY.with(|r| r.borrow().fiber_id_for(tcb))
}

// ---------------------------------------------------------------------------
// Scheduler lock
// ---------------------------------------------------------------------------

thread_local! {
    /// Zephyr scheduler lock nesting count.
    /// When > 0, the Rust scheduler must not switch to a different thread.
    static SCHED_LOCK: Cell<u32> = const { Cell::new(0) };
}

/// Lock the scheduler — prevent thread switching.
pub fn zephyr_sched_lock() {
    SCHED_LOCK.with(|c| c.set(c.get().saturating_add(1)));
}

/// Unlock the scheduler — allow thread switching again.
pub fn zephyr_sched_unlock() {
    SCHED_LOCK.with(|c| c.set(c.get().saturating_sub(1)));
}

/// Whether the scheduler is currently locked.
pub fn is_zephyr_sched_locked() -> bool {
    SCHED_LOCK.with(|c| c.get() > 0)
}

// ---------------------------------------------------------------------------
// Current thread tracking
// ---------------------------------------------------------------------------

thread_local! {
    /// The Zephyr TCB pointer of the currently executing thread.
    /// Set by the Rust scheduler before resuming a fiber, cleared after.
    static CURRENT_ZEPHYR_TCB: Cell<usize> = const { Cell::new(0) };
}

/// Set the current Zephyr TCB pointer.
pub fn set_current_zephyr_tcb(tcb: usize) {
    CURRENT_ZEPHYR_TCB.with(|c| c.set(tcb));
}

/// Get the current Zephyr TCB pointer.
pub fn get_current_zephyr_tcb() -> usize {
    CURRENT_ZEPHYR_TCB.with(|c| c.get())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sched_lock_nesting() {
        assert!(!is_zephyr_sched_locked());
        zephyr_sched_lock();
        assert!(is_zephyr_sched_locked());
        zephyr_sched_lock();
        zephyr_sched_unlock();
        assert!(is_zephyr_sched_locked());
        zephyr_sched_unlock();
        assert!(!is_zephyr_sched_locked());
    }

    #[test]
    fn test_register_lookup() {
        let mut reg = ZephyrThreadRegistry::new();
        reg.register(0x1000, 1);
        reg.register(0x2000, 2);
        assert_eq!(reg.fiber_id_for(0x1000), Some(1));
        assert_eq!(reg.fiber_id_for(0x2000), Some(2));
        assert_eq!(reg.fiber_id_for(0x3000), None);
    }

    #[test]
    fn test_sched_lock_underflow_safe() {
        // Should not panic
        zephyr_sched_unlock();
        assert!(!is_zephyr_sched_locked());
    }

    #[test]
    fn test_current_tcb_default_zero() {
        assert_eq!(get_current_zephyr_tcb(), 0);
    }

    #[test]
    fn test_current_tcb_set_get() {
        set_current_zephyr_tcb(0xDEAD);
        assert_eq!(get_current_zephyr_tcb(), 0xDEAD);
        // Reset for other tests
        set_current_zephyr_tcb(0);
    }
}
