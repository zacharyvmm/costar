/*
 * zephyr_arch.c — Simulator architecture port for Zephyr
 *
 * Implements the arch_* functions that Zephyr's kernel calls.
 * All actual context switching is delegated to the Rust fiber runtime
 * via the sim_abi.h interface (and the zephyr-specific extensions in
 * sim_zephyr_abi.h).
 *
 * In a real Zephyr build, this file would live in:
 *   zephyr/arch/sim/core/sim_arch.c
 *
 * For the standalone test, it's compiled directly through the `cc` crate.
 */

#include "zephyr_arch.h"
#include "sim_abi.h"

#include <stddef.h>

/* ─────────────────────────────────────────────────────────────────────
 * arch_switch
 *
 * Zephyr calls this to switch from the current thread (stored in
 * *switch_from) to a new thread (switch_to).
 *
 * In a real hardware port, this function:
 *   1. Saves callee-saved registers onto the current stack
 *   2. Saves the current stack pointer in *switch_from
 *   3. Loads the new thread's stack pointer
 *   4. Restores callee-saved registers from the new stack
 *   5. Returns into the new thread
 *
 * In the simulator, corosensei handles all register save/restore
 * transparently during suspend()/resume().  The actual thread
 * selection happens in the Rust scheduler, which calls
 * sim_set_current_thread() before resuming the target fiber.
 *
 * This function is a no-op here — the yield happens via
 * sim_port_yield() in the cooperative path, or the scheduler
 * simply resumes the next fiber.
 *
 * NOTE: arch_switch is called with interrupts LOCKED in Zephyr.
 * In our simulator, critical nesting is handled separately
 * and we don't need to simulate the lock here.
 * ──────────────────────────────────────────────────────────────────── */

void arch_switch(void *switch_to, void **switch_from)
{
    (void)switch_to;
    (void)switch_from;

    /* The actual context switch is orchestrated by the Rust scheduler.
     * When the scheduler resumes a fiber:
     *   1. It calls sim_set_current_thread(tcb) to update Zephyr's
     *      _kernel.current pointer
     *   2. It calls corosensei::resume() to switch stacks
     *
     * For the cooperative yield path (arch_yield), the thread calls
     * sim_port_yield() and the scheduler picks the next runnable
     * thread on the next iteration.
     *
     * This no-op is valid because:
     *   - The Zephyr kernel already updated _kernel.current before
     *     calling arch_switch()
     *   - The actual stack switch happens when the Rust scheduler
     *     resumes the fiber corresponding to the new _kernel.current
     */
}

/* ─────────────────────────────────────────────────────────────────────
 * arch_irq_lock / arch_irq_unlock
 *
 * Zephyr uses a lock/unlock pair with an opaque key.
 * We map this to our nesting counter, which returns 0 or 1
 * as the "key" (1 = already locked, 0 = not locked).
 * ──────────────────────────────────────────────────────────────────── */

unsigned int arch_irq_lock(void)
{
    /* Read current nesting before incrementing.
     * sim_enter_critical increments, so we need to capture the
     * current state first. */
    /* We store the current state and then increment.
     * Actually, sim_enter_critical just increments the nesting counter.
     * The key convention: 0 = was already locked, 1 = was unlocked. */
    /* Approach: sim_enter_critical always succeeds.  We return
     * a dummy key that arch_irq_unlock consumes.  Since our critical
     * section model is a simple nesting counter, the key doesn't
     * carry state — pairing is enforced by convention. */
    sim_enter_critical();
    return 0; /* key unused by simulator impl */
}

void arch_irq_unlock(unsigned int key)
{
    (void)key;
    sim_exit_critical();
}

/* ─────────────────────────────────────────────────────────────────────
 * arch_k_cycle_get_32
 *
 * Returns a 32-bit cycle counter.  In the simulator, we read the
 * lower 32 bits of the virtual time.
 * ──────────────────────────────────────────────────────────────────── */

uint32_t arch_k_cycle_get_32(void)
{
    return (uint32_t)(sim_now_ticks() & 0xFFFFFFFFu);
}

/* ─────────────────────────────────────────────────────────────────────
 * arch_system_halt
 *
 * Called on fatal errors.  Records a fatal trace event and returns.
 * ──────────────────────────────────────────────────────────────────── */

void arch_system_halt(unsigned int reason)
{
    (void)reason;
    /* Record a fatal error in the trace. */
    sim_trace_u32("zephyr_fatal", reason);

    /* In a real implementation, we would signal the Rust scheduler
     * to terminate the simulation.  For now, just exit the current
     * fiber (the scheduler will see a Faulted state). */
    sim_task_exit();
}

/* ─────────────────────────────────────────────────────────────────────
 * arch_new_thread
 *
 * Initialize a new thread's stack frame.
 *
 * In hardware ports, this writes an initial exception frame (callee-
 * saved registers, return address = thread_entry, etc.) onto the
 * stack so that when arch_switch() "returns" into this thread, it
 * starts executing thread_entry.
 *
 * In the simulator, corosensei handles the initial context, so this
 * is a no-op.  The stack pointer is unused — the actual coroutine
 * stack is allocated by Fiber::new().
 * ──────────────────────────────────────────────────────────────────── */

void *arch_new_thread(
    void *stack_ptr,
    void *thread_entry,
    void *arg1,
    void *arg2,
    void *arg3
)
{
    (void)thread_entry;
    (void)arg1;
    (void)arg2;
    (void)arg3;

    /* Return the stack pointer unchanged.  The real initial context
     * is set up by Fiber::new() in the Rust runtime. */
    return stack_ptr;
}

/* ─────────────────────────────────────────────────────────────────────
 * arch_yield
 *
 * Cooperative yield — the current thread voluntarily gives up the CPU.
 *
 * In hardware ports, this calls the scheduler to select the next
 * thread and then arch_switch()s to it.
 *
 * In the simulator, this maps to sim_port_yield(), which suspends
 * the active fiber.  The Rust scheduler resumes the next runnable
 * fiber on the next iteration.
 * ──────────────────────────────────────────────────────────────────── */

void arch_yield(void)
{
    sim_port_yield();
}
