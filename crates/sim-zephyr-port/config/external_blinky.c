/* external_blinky.c — Minimal external Zephyr app for golden trace testing.
 *
 * Demonstrates compiling an external .c file via ZEPHYR_APP_SOURCES.
 * Produces a deterministic trace with sim_trace_u32 events.
 *
 * IMPORTANT: must be named zephyr_app_main() to match the init.c rename.
 */
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>
#include "sim_abi.h"

int zephyr_app_main(void)
{
    printk("external_blinky: hello\n");

    sim_trace_u32("INIT", 1);

    /* Brief sleep to let the kernel time-advance machinery run.
     * Without any k_sleep, Zephyr's scheduler can deadlock
     * when the only thread returns immediately. */
    k_sleep(K_MSEC(1));

    sim_trace_u32("TICK", 0);
    sim_trace_u32("DONE", 99);

    return 0;
}
