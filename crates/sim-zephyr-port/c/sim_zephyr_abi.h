// sim_zephyr_abi.h — Zephyr-specific ABI extensions for the simulator.
//
// These functions are called by Zephyr application/firmware code and
// implemented in Rust (sim-ffi).  They provide the glue between Zephyr's
// threading model (3-arg entry points, scheduler lock, k_sleep) and
// the Rust fiber runtime.

#ifndef SIM_ZEPHYR_ABI_H
#define SIM_ZEPHYR_ABI_H

#include <stdint.h>
#include <stddef.h>
#include "sim_abi.h"

#ifdef __cplusplus
extern "C" {
#endif

// ── Thread management ──────────────────────────────────────────────

// Zephyr thread entry takes three void* arguments (unlike FreeRTOS's one).
typedef void (*zephyr_thread_entry_t)(void *, void *, void *);

// Opaque thread handle returned by sim_zephyr_register_thread.
typedef uintptr_t zephyr_tid_t;

// Initialize the Zephyr simulator adapter (thread registry, scheduler state).
void sim_zephyr_init(void);

// Register a Zephyr thread with the Rust fiber runtime.
// Returns an opaque thread handle, or 0 on failure.
zephyr_tid_t sim_zephyr_register_thread(
    const char *name,
    zephyr_thread_entry_t entry,
    void *arg1,
    void *arg2,
    void *arg3,
    uint32_t stack_size,
    uint32_t priority
);

// Set/get the current Zephyr thread TCB pointer.
// Used by the Rust scheduler to synchronize Zephyr's _kernel.current
// with the fiber being resumed.
void sim_zephyr_set_current_thread(void *tcb);
void *sim_zephyr_get_current_thread(void);

// ── Scheduler lock ─────────────────────────────────────────────────
//
// Zephyr's k_sched_lock() / k_sched_unlock() prevent the scheduler
// from switching threads.  In our model, the Rust scheduler checks
// the lock before selecting a new thread to run.

void sim_zephyr_sched_lock(void);
void sim_zephyr_sched_unlock(void);

// ── Scheduler entry ────────────────────────────────────────────────

// Enter the Rust Zephyr scheduler loop.  Does not return.
void sim_zephyr_start_scheduler(void);

// ── Convenience wrappers for standalone tests (no Zephyr SDK) ─────

// Create and start a Zephyr thread (combines register + immediate start).
// Returns the thread handle.
zephyr_tid_t zephyr_thread_spawn(
    const char *name,
    zephyr_thread_entry_t entry,
    void *arg1,
    void *arg2,
    void *arg3,
    uint32_t stack_size,
    int prio
);

// Sleep for a relative number of simulator ticks.
// Delegates to sim_task_delay_until(sim_now_ticks() + ticks).
void zephyr_sleep(uint32_t ticks);

#ifdef __cplusplus
}
#endif

#endif // SIM_ZEPHYR_ABI_H
