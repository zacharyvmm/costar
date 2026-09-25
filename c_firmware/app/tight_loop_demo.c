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
 *   Watchdog task (priority 2) — records a trace message, then sleeps
 *                                for one tick.
 *
 * With edge instrumentation enabled (SIM_INSTRUMENT_EDGES=1 + Clang):
 *   The burner's loop back-edge triggers __sanitizer_cov_trace_pc_guard
 *   → sim_budget_poll → BudgetExceeded yield, which the simulator treats
 *   as one tick of CPU time (a tick interrupt).  When the watchdog's delay
 *   expires, FreeRTOS preempts the burner with the higher-priority
 *   watchdog.  The trace shows budget_exceeded / watchdog_alive
 *   interleaving.
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
 *   one budget yield per ~50 000 edges; the exact count depends on how
 *   many edges the compiler emits per iteration.
 *
 */
static void vBurnerTask(void *pvParameters)
{
    (void)pvParameters;
    volatile uint64_t counter = 0;

    while (counter < 5000000) {
        counter++;
    }

    sim_trace_u32("burner_done", (uint32_t)(counter & 0xFFFFFFFFu));
    vTaskDelete(NULL);
}

/* ── Watchdog task: preempts the burner every tick ─────────────────
 *
 * Sleeps one tick per round.  Time only advances while the burner runs
 * because the burner's exhausted budget counts as a tick, so each wake-up
 * preempts the burner.  After WATCHDOG_ROUNDS rounds it exits, letting the
 * burner finish unimpeded.
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
        /* A higher-priority task that merely yielded would be selected
         * again straight away; sleeping lets the burner run. */
        vTaskDelay(1);
    }
    vTaskDelete(NULL);
}

/* ── Entry point (called by sim-runner --mode tight-loop) ───────── */

int c_sim_tight_loop_main(void)
{
    TaskHandle_t thBurner, thWatchdog;

    /* Aggressive budget: yield after 5 budget-poll calls.
     * With EDGE_CHECK_INTERVAL=10 000, that's ~50 000 edges
     * per yield, or ~17 000 loop iterations at ~3 edges/iter. */
    sim_budget_set_limit(5);

    xTaskCreate(vBurnerTask,   "Burner",   256, NULL, 1, &thBurner);
    xTaskCreate(vWatchdogTask, "Watchdog", 256, NULL, 2, &thWatchdog);

    vTaskStartScheduler();
    return 0;
}
