//! # sim-zephyr-port
//!
//! Rust side of the custom Zephyr simulator port.
//!
//! The C side (zephyr_arch.c, zephyr_arch.h, and Zephyr board files)
//! is compiled via the `cc` crate in `build.rs` for standalone testing,
//! or built externally via `west build -b sim` for real Zephyr kernels.
//!
//! This Rust side wires the Zephyr arch port hooks to the sim-ffi ABI
//! and the fiber runtime — analogous to how sim-freertos-port wires
//! FreeRTOS hooks.
//!
//! # Architecture
//!
//! Zephyr threads map to Rust-managed corosensei fibers, just like
//! FreeRTOS tasks.  The key differences:
//!
//! - Zephyr has an O(1) priority scheduler (bitmap-based) vs FreeRTOS's
//!   linked-list round-robin scheduler.
//! - Zephyr threads are statically defined (K_THREAD_DEFINE) vs
//!   FreeRTOS's dynamic xTaskCreate.
//! - Zephyr has a multi-stage init sequence (PRE_KERNEL_1, POST_KERNEL,
//!   APPLICATION) where threads are created at different levels.
//! - Zephyr uses k_msleep (milliseconds) vs FreeRTOS's vTaskDelay (ticks).
//!
//! The scheduler loop in sim_start_scheduler() already supports
//! priority-ordered task selection; for Zephyr we add a secondary
//! scheduler lock and thread registry.

use std::cell::RefCell;
use std::collections::BTreeMap;

use sim_fiber::TaskId;

// ---------------------------------------------------------------------------
// Zephyr thread registry
// ---------------------------------------------------------------------------

/// A registered Zephyr thread.
#[derive(Debug, Clone)]
pub struct ZephyrThread {
    /// Rust fiber task ID.
    pub task_id: TaskId,
    /// The TCB pointer (struct k_thread *) — opaque from Rust side.
    pub tcb: usize,
    /// Thread name.
    pub name: &'static str,
    /// Zephyr priority (lower = higher priority).
    pub priority: i32,
}

// Thread-local registry mapping TCB pointers to ZephyrThread metadata.
thread_local! {
    static ZEPHYR_THREADS: RefCell<BTreeMap<usize, ZephyrThread>> =
        const { RefCell::new(BTreeMap::new()) };
}

// The current Zephyr thread TCB, if any.
// Set by sim_zephyr_set_current_thread, read by sim_zephyr_get_current_thread.
thread_local! {
    static CURRENT_ZEPHYR_TCB: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

// Scheduler lock nesting counter.
// When > 0, the scheduler should not switch threads.
thread_local! {
    static ZEPHYR_SCHED_LOCK: std::cell::Cell<u32> =
        const { std::cell::Cell::new(0) };
}

// Whether Zephyr adapter has been initialized.
thread_local! {
    static ZEPHYR_INITIALIZED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Initialize the Zephyr adapter.
pub fn init() {
    ZEPHYR_INITIALIZED.with(|init| init.set(true));
}

/// Whether the Zephyr adapter has been initialized.
pub fn is_initialized() -> bool {
    ZEPHYR_INITIALIZED.with(|init| init.get())
}

/// Register a Zephyr thread in the registry.
pub fn register_tcb(task_id: TaskId, tcb: usize, name: &'static str, priority: i32) {
    ZEPHYR_THREADS.with(|threads| {
        threads.borrow_mut().insert(
            tcb,
            ZephyrThread {
                task_id,
                tcb,
                name,
                priority,
            },
        );
    });
}

/// Look up a Zephyr thread by TCB pointer.
pub fn find_by_tcb(tcb: usize) -> Option<ZephyrThread> {
    ZEPHYR_THREADS.with(|threads| threads.borrow().get(&tcb).cloned())
}

/// Look up a Zephyr thread by Rust task ID.
pub fn find_by_task_id(task_id: TaskId) -> Option<ZephyrThread> {
    ZEPHYR_THREADS.with(|threads| {
        threads
            .borrow()
            .values()
            .find(|t| t.task_id == task_id)
            .cloned()
    })
}

/// Set the currently-executing Zephyr TCB.
pub fn set_current_tcb(tcb: usize) {
    CURRENT_ZEPHYR_TCB.with(|c| c.set(tcb));
}

/// Get the currently-executing Zephyr TCB (0 if none).
pub fn current_tcb() -> usize {
    CURRENT_ZEPHYR_TCB.with(|c| c.get())
}

/// Lock the scheduler (increment nesting counter).
pub fn sched_lock() {
    ZEPHYR_SCHED_LOCK.with(|c| c.set(c.get().saturating_add(1)));
}

/// Unlock the scheduler (decrement nesting counter).
pub fn sched_unlock() {
    ZEPHYR_SCHED_LOCK.with(|c| c.set(c.get().saturating_sub(1)));
}

/// Whether the Zephyr scheduler is currently locked.
pub fn is_sched_locked() -> bool {
    ZEPHYR_SCHED_LOCK.with(|c| c.get() > 0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zephyr_init_and_register() {
        init();
        assert!(is_initialized());

        register_tcb(1, 0x1000, "test_thread", 7);
        let found = find_by_tcb(0x1000);
        assert!(found.is_some());
        let t = found.unwrap();
        assert_eq!(t.task_id, 1);
        assert_eq!(t.name, "test_thread");
        assert_eq!(t.priority, 7);

        let found2 = find_by_task_id(1);
        assert!(found2.is_some());
        assert_eq!(found2.unwrap().tcb, 0x1000);
    }

    #[test]
    fn test_zephyr_sched_lock() {
        assert!(!is_sched_locked());

        sched_lock();
        assert!(is_sched_locked());

        sched_lock();
        assert!(is_sched_locked());

        sched_unlock();
        assert!(is_sched_locked());

        sched_unlock();
        assert!(!is_sched_locked());
    }

    #[test]
    fn test_zephyr_current_tcb() {
        assert_eq!(current_tcb(), 0);

        set_current_tcb(0xDEAD);
        assert_eq!(current_tcb(), 0xDEAD);

        set_current_tcb(0);
        assert_eq!(current_tcb(), 0);
    }

    #[test]
    fn test_zephyr_find_missing() {
        init();
        assert!(find_by_tcb(0xBAD).is_none());
        assert!(find_by_task_id(999).is_none());
    }

    #[test]
    fn test_zephyr_multiple_threads() {
        init();
        register_tcb(1, 0x1000, "t1", 1);
        register_tcb(2, 0x2000, "t2", 2);
        register_tcb(3, 0x3000, "t3", 3);

        assert_eq!(ZEPHYR_THREADS.with(|t| t.borrow().len()), 3);

        // Priority ordering
        let mut threads: Vec<_> = ZEPHYR_THREADS.with(|t| t.borrow().values().cloned().collect());
        threads.sort_by_key(|t| t.priority);
        assert_eq!(threads[0].name, "t1");
        assert_eq!(threads[1].name, "t2");
        assert_eq!(threads[2].name, "t3");
    }
}
