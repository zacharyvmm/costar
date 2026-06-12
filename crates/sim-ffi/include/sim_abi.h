#ifndef SIM_ABI_H
#define SIM_ABI_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Task entry point ──────────────────────────────────────────────── */

/** Signature of a simulated task entry function. */
typedef void (*sim_task_entry_fn)(void *arg);

/* ── Yield / resume reasons ────────────────────────────────────────── */

typedef enum sim_yield_reason {
    SIM_YIELD_COOPERATIVE = 0,
    SIM_YIELD_RTOS_PORT   = 1,
    SIM_YIELD_BLOCKED     = 2,
    SIM_YIELD_SLEEP       = 3,
    SIM_YIELD_IO          = 4,
    SIM_YIELD_TASK_EXIT   = 5,
} sim_yield_reason_t;

/* ── Opaque handles ────────────────────────────────────────────────── */

/** Opaque task handle returned by sim_create_task. */
typedef uintptr_t sim_task_handle_t;

/* ── Virtual time ──────────────────────────────────────────────────── */

/** Return the current virtual time in ticks. */
uint64_t sim_now_ticks(void);

/* ── Task lifecycle ────────────────────────────────────────────────── */

/**
 * Register a new simulated task with the simulator.
 *
 * The task does NOT start running until sim_start_scheduler() is called.
 *
 * @param name                  Human-readable task name (must be a string
 *                              literal or permanently allocated).
 * @param entry                 C function to execute as the task body.
 * @param arg                   Argument passed to the task entry.
 * @param requested_stack_words Stack depth requested by the RTOS (in
 *                              words).  The simulator may allocate a
 *                              larger host stack internally.
 * @param priority              RTOS task priority (0 = lowest).
 * @return An opaque task handle, or 0 on failure.
 */
sim_task_handle_t sim_create_task(
    const char *name,
    sim_task_entry_fn entry,
    void *arg,
    uint32_t requested_stack_words,
    uint32_t priority
);

/**
 * Start the simulator scheduler.
 *
 * This function never returns in a running simulation; control stays
 * inside the Rust event loop until the simulation terminates.
 */
void sim_start_scheduler(void);

/**
 * Yield the currently executing task.
 *
 * Must only be called from within a running task.  If no task is
 * active the call is recorded as a fatal error.
 */
void sim_port_yield(void);

/**
 * Mark the current task as exited.
 *
 * The task will not be rescheduled after this call.
 */
void sim_task_exit(void);

/* ── Critical sections ─────────────────────────────────────────────── */

/** Enter a virtual critical section (nesting counter). */
void sim_enter_critical(void);

/** Exit a virtual critical section.  Deferred interrupts are delivered
 *  when nesting reaches zero. */
void sim_exit_critical(void);

/* ── Trace helpers ──────────────────────────────────────────────────── */

/** Record a u32 data point in the simulator trace. */
void sim_trace_u32(const char *label, uint32_t value);

#ifdef __cplusplus
}
#endif

#endif /* SIM_ABI_H */
