/*
 * Simulator hooks — sim_hooks.c
 *
 * This file provides the C-side implementation of the simulator
 * hooks declared in sim_abi.h.  These are thin wrappers around
 * the Rust #[no_mangle] exports, used when the Rust functions
 * need C-callable wrappers.
 *
 * For the MVP, sim_abi.h functions are called directly from port.c
 * and the FreeRTOS kernel.  This file exists as a placeholder for
 * future trampolines and C-side helpers.
 */

#include "sim_abi.h"

/*
 * Optional: A C-side task creation helper that reads metadata from
 * the FreeRTOS stack and calls sim_create_task.
 *
 * Currently unused — the scheduler does this directly in Rust.
 * Kept for reference.
 */
#if 0
#include "portmacro.h"
#include <stddef.h>

sim_task_handle_t sim_port_create_task_from_stack(
    StackType_t *pxStack,
    const char *name,
    uint32_t stack_depth_words,
    uint32_t priority
)
{
    StackType_t *base = &pxStack[0]; /* metadata at lowest address */

    if (base[0] != PORT_MAGIC) {
        return 0; /* invalid stack */
    }

    sim_task_entry_fn entry = (sim_task_entry_fn)(uintptr_t)base[1];
    void *arg                = (void *)(uintptr_t)base[2];

    return sim_create_task(name, entry, arg, stack_depth_words, priority);
}
#endif
