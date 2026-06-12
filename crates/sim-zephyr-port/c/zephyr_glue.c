// zephyr_glue.c — Convenience wrappers bridging the Zephyr-like API
// to the Rust simulator ABI.
//
// These are thin C wrappers, not Rust #[no_mangle] exports.
// They're compiled as part of the Zephyr C payload.

#include "sim_zephyr_abi.h"
#include "sim_abi.h"
#include <stddef.h>

zephyr_tid_t zephyr_thread_spawn(
    const char *name,
    zephyr_thread_entry_t entry,
    void *arg1,
    void *arg2,
    void *arg3,
    uint32_t stack_size,
    int prio)
{
    (void)stack_size;
    return sim_zephyr_register_thread(name, entry, arg1, arg2, arg3, stack_size, (uint32_t)prio);
}

void zephyr_sleep(uint32_t ticks)
{
    uint64_t now = sim_now_ticks();
    sim_task_delay_until(now + (uint64_t)ticks);
}
