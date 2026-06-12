// zephyr_arch.c — Simulator arch port for Zephyr.
//
// Replaces the POSIX-thread-based native_sim arch layer with
// corosensei stackful fibers and virtual interrupt masking.

#include "zephyr_arch.h"
#include "sim_abi.h"

void arch_switch(void *switch_to, void **switch_from)
{
    (void)switch_to;
    (void)switch_from;
    // No-op: the actual context switch is performed by corosensei
    // when the Rust scheduler resumes the fiber for the selected
    // thread.  The arch_switch call in Zephyr's scheduler is a
    // signal that a context switch should happen, but in our model
    // it always happens when the scheduler yields via sim_port_yield.
}

unsigned int arch_irq_lock(void)
{
    // Return a dummy key (not used by sim_exit_critical / sim_enter_critical).
    sim_enter_critical();
    return 0;
}

void arch_irq_unlock(unsigned int key)
{
    (void)key;
    sim_exit_critical();
}

unsigned int arch_k_cycle_get_32(void)
{
    return (unsigned int)sim_now_ticks();
}
