// zephyr_arch.h — Zephyr arch port declarations for the simulator.
//
// These are the arch-level functions that Zephyr's kernel calls for
// context switching, interrupt masking, and cycle counting.  In the
// simulator, they delegate to the Rust fiber runtime via sim_abi.h
// and sim_zephyr_abi.h.

#ifndef ZEPHYR_ARCH_H
#define ZEPHYR_ARCH_H

#ifdef __cplusplus
extern "C" {
#endif

// Context switch — no-op in the simulator; the actual stack switch
// is handled by corosensei via the Rust scheduler.
void arch_switch(void *switch_to, void **switch_from);

// Interrupt lock/unlock — delegates to sim_enter_critical / sim_exit_critical.
unsigned int arch_irq_lock(void);
void arch_irq_unlock(unsigned int key);

// Cycle counter — returns sim_now_ticks().
unsigned int arch_k_cycle_get_32(void);

#ifdef __cplusplus
}
#endif

#endif // ZEPHYR_ARCH_H
