/*
 * zephyr_arch.h — Simulator architecture port for Zephyr
 *
 * This header provides the interface that Zephyr's kernel expects from
 * an architecture port.  When building Zephyr with `west build -b sim`,
 * Zephyr's arch/ layer includes this header (or equivalent) to access
 * the thread-switch, IRQ-lock, and timing primitives.
 *
 * In a real Zephyr build, this file lives in:
 *   zephyr/arch/sim/include/sim_arch.h
 * and is included by the kernel via:
 *   #include <arch/sim/sim_arch.h>
 *
 * For the standalone test (compiled through `cc`), this header is
 * included directly by the test app.
 */

#ifndef ZEPHYR_ARCH_H
#define ZEPHYR_ARCH_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Thread context switch ─────────────────────────────────────────── */

/**
 * Switch from the current thread to `switch_to`.
 *
 * In Zephyr's native arch ports, this saves callee-saved registers,
 * switches the stack pointer, and returns into the new thread.
 *
 * In the simulator, this is a no-op: corosensei handles all stack
 * switching transparently.  The actual thread selection happens in
 * the Rust scheduler, which calls sim_set_current_thread() before
 * resuming the target fiber.
 *
 * @param switch_to    The `struct k_thread *` to switch to.
 * @param switch_from  Output: receives the previous `k_thread *`.
 */
void arch_switch(void *switch_to, void **switch_from);

/* ── Interrupt lock / unlock ──────────────────────────────────────── */

/**
 * Lock interrupts and return the previous interrupt key.
 *
 * In the simulator, this maps to a nesting counter via
 * sim_enter_critical().  This is safe to call from any context.
 *
 * @return An opaque key that must be passed to arch_irq_unlock().
 */
unsigned int arch_irq_lock(void);

/**
 * Unlock interrupts using the key returned by arch_irq_lock().
 *
 * Deferred virtual interrupts are delivered when the nesting count
 * reaches zero.
 *
 * @param key  The key returned by a matching arch_irq_lock().
 */
void arch_irq_unlock(unsigned int key);

/* ── Cycle counter ────────────────────────────────────────────────── */

/**
 * Return the current hardware cycle count.
 *
 * Mapped to sim_now_ticks() in the simulator.  Zephyr uses this for
 * k_cycle_get_32() and k_uptime_get().
 */
uint32_t arch_k_cycle_get_32(void);

/* ── System halt ──────────────────────────────────────────────────── */

/**
 * Halt the system (fatal error).
 *
 * Records a fatal trace event and stops the simulation.
 */
void arch_system_halt(unsigned int reason);

/* ── Thread initialization ────────────────────────────────────────── */

/**
 * Initialize a new thread's stack frame so it starts executing
 * `thread_entry` when first switched to.
 *
 * In the simulator, this is a no-op because corosensei handles the
 * initial context.  The entry-point-to-fiber mapping is handled by
 * the Rust bridge (sim_zephyr_register_thread).
 *
 * @param stack_ptr      Pointer to the top of the thread's stack.
 * @param thread_entry   The thread entry function.
 * @param arg1           First argument to thread_entry.
 * @param arg2           Second argument to thread_entry.
 * @param arg3           Third argument to thread_entry.
 * @return               The new stack pointer after initialization.
 */
void *arch_new_thread(
    void *stack_ptr,
    void *thread_entry,
    void *arg1,
    void *arg2,
    void *arg3
);

/* ── Cooperative context switch ───────────────────────────────────── */

/**
 * Yield cooperatively from the current thread.
 *
 * Equivalent to arch_switch() with the scheduler selecting the next
 * thread.  Maps to sim_port_yield().
 */
void arch_yield(void);

#ifdef __cplusplus
}
#endif

#endif /* ZEPHYR_ARCH_H */
