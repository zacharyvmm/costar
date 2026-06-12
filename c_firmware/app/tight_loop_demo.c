#include "FreeRTOS.h"
#include "task.h"
#include "sim_abi.h"

/* ────────────────────────────────────────────────────────────────────
 * Tier 3 edge-instrumentation demo ("tight-loop" mode)
 *
 * Proves that basic-block edge instrumentation
 * (-fsanitize-coverage=trace-pc-guard) can preempt a tight while(1){}
 * loop that never calls any RTOS function.
 *
 * Architecture:
 *   Burner task   (priority 1) — tight volatile-counter loop.
 *   Watchdog task (priority 2) — prints a trace message each time
 *                                the scheduler picks it, then yields.
 *
 * With edge instrumentation enabled (SIM_INSTRUMENT_EDGES=1 + Clang):
 *   The burner's loop back-edge triggers __sanitizer_cov_trace_pc_guard
 *   → sim_budget_poll → BudgetExceeded yield.  The scheduler then
 *   selects the higher-priority watchdog.  The trace shows repeated
 *   budget_exceeded / watchdog_alive interleaving.
 *
 * Without edge instrumentation:
 *   The burner runs forever — the simulator hangs (caught by the
 *   wall-clock watchdog if one is configured).
 * ──────────────────────────────────────────────────────────────────── */

/* ── Burner task: pure CPU-bound tight loop ────────────────────────
 *
 * This task increments a volatile counter in a tight loop that
 * contains NO function calls, NO RTOS primitives, and NO manual
 * SIM_LOOP_POLL() placements.  It is the canonical case that only
 * Tier 3 edge instrumentation can preempt.
 *
 * 5 000 000 iterations at ~3 edges each = 15M edges.
 * With EDGE_CHECK_INTERVAL=10 000 and budget max_entries=5:
 *   15M / (10 000 * 5) ≈ 300 yields.
 *
 * The function returns normally (rather than calling sim_task_exit())
 * so the coroutine stack unwinds cleanly.  sim_task_exit() would leave
 * C frames on the coroutine stack, causing a corosensei force-unwind
 * panic during Fiber::drop at process exit.
 */
static void vBurnerTask(void *pvParameters)
{
    (void)pvParameters;
    volatile uint64_t counter = 0;

    while (counter < 5000000) {
        counter++;
    }

    sim_trace_u32("burner_done", (uint32_t)(counter & 0xFFFFFFFFu));
    /* Return normally — coroutine stack unwinds cleanly. */
}

/* ── Watchdog task: runs when burner is preempted ──────────────────
 *
 * Each time the burner exhausts its budget, the scheduler selects
 * this task (it has higher priority).  It records a trace message
 * and yields cooperatively.  After WATCHDOG_ROUNDS iterations it
 * exits, letting the burner finish unimpeded.
 */
static uint32_t g_watchdog_count = 0;
#define WATCHDOG_ROUNDS 10

static void vWatchdogTask(void *pvParameters)
{
    (void)pvParameters;
    uint32_t round;

    for (round = 0; round < WATCHDOG_ROUNDS; round++) {
        g_watchdog_count++;
        sim_trace_u32("watchdog_alive", g_watchdog_count);
        /* Yield cooperatively (→ RtosPortYield → Suspended).
         * The scheduler selects the burner next (lower priority);
         * when the burner next exhausts its budget, the scheduler
         * selects us again and we resume from here. */
        sim_port_yield();
    }
    /* After WATCHDOG_ROUNDS yields, the function returns normally.
     * The coroutine wrapper marks the fiber as Exited. */
}

/* ── FreeRTOS memory hooks ────────────────────────────────────────
 *
 * vApplicationGetIdleTaskMemory and vApplicationGetTimerTaskMemory
 * are defined in main.c.  We rely on those single definitions to
 * avoid duplicate-symbol linker errors (same pattern as
 * main_interactive.c). */

/* ── Entry point (called by sim-runner --mode tight-loop) ───────── */

int c_sim_tight_loop_main(void)
{
    TaskHandle_t thBurner, thWatchdog;
    sim_task_handle_t hBurner, hWatchdog;

    /* Aggressive budget: yield after 5 budget-poll calls.
     * With EDGE_CHECK_INTERVAL=10 000, that's ~50 000 edges
     * per yield, or ~17 000 loop iterations at ~3 edges/iter. */
    sim_budget_set_limit(5);

    /* Create FreeRTOS TCBs (needed for vTaskStartScheduler to work).
     * The Rust scheduler uses its own priority-based selection. */
    xTaskCreate(vBurnerTask,   "Burner",   256, NULL, 1, &thBurner);
    xTaskCreate(vWatchdogTask, "Watchdog", 256, NULL, 2, &thWatchdog);

    /* Create Rust fibers (directly, not via trace hook). */
    hBurner = sim_create_task(
        "Burner", (sim_task_entry_fn)vBurnerTask, NULL, 256, 1);
    hWatchdog = sim_create_task(
        "Watchdog", (sim_task_entry_fn)vWatchdogTask, NULL, 256, 2);

    /* Register TCB mappings so sim_set_current_task_by_id works. */
    sim_bridge_register(hBurner, thBurner);
    sim_bridge_register(hWatchdog, thWatchdog);

    /* Transfer control to the Rust fiber scheduler. */
    vTaskStartScheduler();
    return 0;
}
