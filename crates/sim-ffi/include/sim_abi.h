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

/**
 * Suspend the current task until the given absolute virtual time.
 *
 * Must only be called from within a running task.
 * The scheduler will not resume this task before `until_ticks`.
 */
void sim_task_delay_until(uint64_t until_ticks);

/* ── Scheduler control (called by Rust) ─────────────────────────────── */

/**
 * Port hook: called by traceTASK_CREATE after FreeRTOS initialises a
 * new TCB.  Creates the corresponding Rust fiber and stores the handle
 * in the TCB.  The parameter is actually a `TCB_t *` (tskTaskControlBlock)
 * but we use void* here to avoid requiring the full struct definition.
 */
void sim_port_task_created(void *pxNewTCB);

/** Register a TCB mapping for sim_set_current_task_by_id. */
void sim_bridge_register(uint64_t task_id, void *tcb);

/** Record a TCB for deferred fiber creation. */
void sim_bridge_add_pending_tcb(void *tcb);

/** Create Rust fibers for all pending TCBs.  Returns count created. */
uint32_t sim_bridge_create_pending_fibers(void);

/**
 * Set the currently-executing TCB by Rust task id.
 *
 * Called by the Rust scheduler before resuming a fiber so that
 * the C kernel's pxCurrentTCB is correct when vTaskDelay / taskYIELD
 * are called.
 */
void sim_set_current_task_by_id(uint64_t task_id);

/**
 * Advance the RTOS tick count by one and move any expired delayed
 * tasks back onto the ready list.
 *
 * Called by the Rust scheduler when virtual time crosses a tick
 * boundary.  Returns the number of tasks woken.
 */
uint32_t sim_tick_advance(void);

/**
 * Batch-advance the tick count by `count` ticks.
 *
 * Semantically equivalent to calling sim_tick_advance() `count` times,
 * but with a single C↔Rust crossing.  Returns the total number of
 * context-switch requests signalled during the batch.
 */
uint32_t sim_advance_ticks(uint32_t count);

/* ── Critical sections ─────────────────────────────────────────────── */

/** Enter a virtual critical section (nesting counter). */
void sim_enter_critical(void);

/** Exit a virtual critical section.  Deferred interrupts are delivered
 *  when nesting reaches zero. */
void sim_exit_critical(void);

/* ── Trace helpers ──────────────────────────────────────────────────── */

/** Record a u32 data point in the simulator trace. */
void sim_trace_u32(const char *label, uint32_t value);

/* ── Interrupt controller ──────────────────────────────────────────── */

/** Raise a virtual interrupt (adds to pending set). */
void sim_irq_raise(uint32_t irq);

/** Clear a pending virtual interrupt (acknowledge). */
void sim_irq_clear(uint32_t irq);

/** Return the lowest pending IRQ number, or UINT32_MAX if none. */
uint32_t sim_irq_pending(void);

/** Deliver all pending interrupts. Returns count delivered. */
uint32_t sim_irq_deliver_pending(uint64_t now);

/* ── Virtual UART ──────────────────────────────────────────────────── */

/** Write bytes to a virtual UART. Returns bytes written. */
uint32_t sim_uart_write(uint32_t id, const uint8_t *data, uint32_t len);

/* ── Virtual timer ──────────────────────────────────────────────────── */

/** Arm a virtual timer to fire after `delay_ticks` from now. */
void sim_timer_arm(uint32_t id, uint64_t delay_ticks);

/** Disarm a virtual timer. */
void sim_timer_disarm(uint32_t id);

/* ── GPIO ───────────────────────────────────────────────────────────── */

/**
 * Set a GPIO pin state.
 * Returns the IRQ number if change triggered an interrupt, or UINT32_MAX.
 */
uint32_t sim_gpio_set(uint32_t id, uint32_t pin, uint32_t state);

/* ── Virtual networking (deterministic) ─────────────────────────────── */

 /** Inject a packet into the network device rx queue. Returns bytes injected. */
 uint32_t sim_net_inject_rx(const uint8_t *data, uint32_t len);

 /** Drain oldest tx packet into buf. Returns bytes written (0 if empty). */
 uint32_t sim_net_drain_tx(uint8_t *buf, uint32_t buf_size);

 /** Check if any rx packets are pending. Returns 1 if yes, 0 if no. */
 uint32_t sim_net_poll(void);

 /* ── Host-connected I/O (interactive mode) ──────────────────────────── */

 /** Register a host file descriptor with the poller. Returns 0 on success. */
 int32_t sim_host_register_fd(int32_t fd);

 /** Deregister a host file descriptor from the poller. Returns 0 on success. */
 int32_t sim_host_deregister_fd(int32_t fd);

 /** Block the current task on a host file descriptor (yields with IoWait). */
 void sim_host_block_on_fd(int32_t fd);

/* ── CPU-bound stall mitigation (budget polling) ──────────────────── */

/**
 * Poll the function-entry budget for the current task.
 *
 * Called from __cyg_profile_func_enter when -finstrument-functions is
 * enabled.  Increments an entry counter; if the budget is exceeded,
 * the fiber yields with BudgetExceeded and resets on resume.
 *
 * Safe to call from any context (uses thread-local state only).
 */
void sim_budget_poll(void);

/**
 * Reset the function-entry budget counter for the current task.
 *
 * Call at task startup to clear any residual budget state from
 * a previous task that ran on the same host thread.
 */
void sim_budget_reset(void);

#ifdef __cplusplus
}
#endif

#endif /* SIM_ABI_H */
