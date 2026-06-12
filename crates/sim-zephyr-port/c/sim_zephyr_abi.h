/*
 * sim_zephyr_abi.h — Zephyr-specific simulator ABI extensions
 *
 * This header extends sim_abi.h with functions specific to the Zephyr
 * adapter.  These functions bridge Zephyr's thread model to the Rust
 * fiber runtime.
 *
 * For a real Zephyr build, this is included from the Zephyr arch port
 * (arch/sim/core/sim_arch.c) and the board init code.
 */

#ifndef SIM_ZEPHYR_ABI_H
#define SIM_ZEPHYR_ABI_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Zephyr thread registration ────────────────────────────────────── */

/**
 * Register a Zephyr thread with the simulator.
 *
 * Creates a Rust fiber for the thread.  The thread body will be
 * executed as a coroutine when the scheduler selects it.
 *
 * This is the Zephyr equivalent of sim_create_task for FreeRTOS.
 * Unlike FreeRTOS, Zephyr threads are typically defined statically
 * via K_THREAD_DEFINE, so this function is called during the Zephyr
 * init sequence (not from application main).
 *
 * @param name         Human-readable thread name.
 * @param entry        Thread entry function (takes 3 void* args).
 * @param arg1         First argument to entry.
 * @param arg2         Second argument to entry.
 * @param arg3         Third argument to entry.
 * @param stack_size   Requested stack size in bytes.
 * @param priority     Thread priority (lower = higher).
 * @return             Opaque task handle, or 0 on failure.
 */
uintptr_t sim_zephyr_register_thread(
    const char *name,
    void (*entry)(void *, void *, void *),
    void *arg1,
    void *arg2,
    void *arg3,
    uint32_t stack_size,
    int32_t priority
);

/**
 * Set the currently-executing Zephyr thread (TCB pointer).
 *
 * Called by the Rust scheduler before resuming a fiber so that
 * Zephyr's _kernel.current is correct.
 *
 * @param tcb  Pointer to the struct k_thread that is about to run.
 */
void sim_zephyr_set_current_thread(void *tcb);

/**
 * Get the current Zephyr thread TCB pointer.
 *
 * @return  The current k_thread pointer, or NULL if none.
 */
void *sim_zephyr_get_current_thread(void);

/**
 * Lock the Zephyr scheduler (prevent thread switching).
 *
 * All calls to arch_switch(), arch_yield(), or sim_port_yield()
 * are suppressed while the scheduler is locked.  The nesting counter
 * supports recursive locking.
 */
void sim_zephyr_sched_lock(void);

/**
 * Unlock the Zephyr scheduler.
 *
 * When the nesting count reaches zero, any pending scheduler
 * operations are processed.
 */
void sim_zephyr_sched_unlock(void);

/* ── Zephyr init ──────────────────────────────────────────────────── */

/**
 * Initialize the Zephyr simulator adapter.
 *
 * Must be called once before any Zephyr threads are registered.
 * Sets up the thread registry and kernel state.
 */
void sim_zephyr_init(void);

/**
 * Start the Zephyr simulator scheduler.
 *
 * Transfers control to the Rust event loop.  This function does not
 * return until the simulation terminates.
 *
 * This replaces sim_start_scheduler() for Zephyr — it uses the same
 * fiber runtime but integrates with Zephyr's priority-based O(1)
 * scheduler rather than FreeRTOS's linked-list scheduler.
 */
void sim_zephyr_start_scheduler(void);

#ifdef __cplusplus
}
#endif

#endif /* SIM_ZEPHYR_ABI_H */
