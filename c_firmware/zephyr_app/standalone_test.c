/* standalone_test.c — Zephyr hello-thread demo (no Zephyr SDK needed).
 *
 * Exercises:
 *   - Multi-thread creation and priority scheduling (blinker + worker)
 *   - zephyr_sleep (sim_task_delay_until — virtual time advancement)
 *   - sim_schedule_event (peripheral timer — k_timer concept)
 *   - sim_schedule_event (deferred work — k_work concept)
 */

#include "sim_zephyr_abi.h"
#include "sim_abi.h"

/* ── Peripheral timer callback (k_timer concept) ──────────────────── */

static void vtimer_callback(void)
{
    sim_trace_u32("vtimer_fired", 1);
}

/* ── Deferred work callback (k_work concept) ──────────────────────── */

static void deferred_work_cb(void)
{
    sim_trace_u32("deferred_work_done", 1);
}

/* ── Thread 1: Blinker (prio 5 — higher) ──────────────────────────── */

static void blinker_entry(void *a, void *b, void *c)
{
    (void)a; (void)b; (void)c;

    sim_trace_u32("zephyr_hello", 1);

    /* Arm a virtual timer at now + 1 tick via the peripheral event queue.
       The callback fires when virtual time reaches now + 1. */
    sim_schedule_event(sim_now_ticks() + 1, vtimer_callback);

    /* Schedule a deferred work item at now + 3 ticks. */
    sim_schedule_event(sim_now_ticks() + 3, deferred_work_cb);

    /* Sleep for 5 ticks.  The timer fires at +1, work at +3, both
       before we wake up at +5. */
    zephyr_sleep(5);

    sim_trace_u32("zephyr_wake", 1);
}

/* ── Thread 2: Worker (prio 3 — lower) ────────────────────────────── */

static void worker_entry(void *a, void *b, void *c)
{
    (void)a; (void)b; (void)c;

    sim_trace_u32("zephyr_worker_start", 1);

    zephyr_sleep(1);

    sim_trace_u32("zephyr_worker_done", 1);
}

/* ── Zephyr entry point ──────────────────────────────────────────── */

int c_zephyr_main(void)
{
    sim_zephyr_init();

    zephyr_thread_spawn("blinker", blinker_entry, NULL, NULL, NULL, 1024, 5);
    zephyr_thread_spawn("worker", worker_entry, NULL, NULL, NULL, 1024, 3);

    sim_zephyr_start_scheduler();

    return 0;
}
