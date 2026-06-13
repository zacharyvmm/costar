/* app_main.c — Zephyr multi-threaded test app for cc crate build.
 *
 * Creates two threads that yield/sleep cooperatively:
 *   - thread_a: priority 5, traces start → sleeps 2000 µs → traces wake → yields
 *   - thread_b: priority 5, traces start → yields → traces done
 *
 * Uses sim_trace_u32() from the simulator ABI to record events,
 * bypassing the Zephyr console buffering issue.
 */

#include <zephyr/kernel.h>
#include "sim_abi.h"

/* Stack size for each thread. */
#define STACK_SIZE 1024

/* Thread priorities. */
#define PRIO_A 5
#define PRIO_B 5

/* ── Thread A ────────────────────────────────────────────────────── */

static void thread_a_entry(void *a, void *b, void *c)
{
    ARG_UNUSED(a);
    ARG_UNUSED(b);
    ARG_UNUSED(c);

    sim_trace_u32("thread_a_start", 1);

    /* Sleep for 2000 µs. */
    k_sleep(K_USEC(2000));

    sim_trace_u32("thread_a_wake", 1);

    /* Yield to let thread B run. */
    k_yield();

    sim_trace_u32("thread_a_yield_done", 1);
}

/* ── Thread B ────────────────────────────────────────────────────── */

static void thread_b_entry(void *a, void *b, void *c)
{
    ARG_UNUSED(a);
    ARG_UNUSED(b);
    ARG_UNUSED(c);

    sim_trace_u32("thread_b_start", 1);

    /* Yield cooperatively — thread_a should resume. */
    k_yield();

    sim_trace_u32("thread_b_done", 1);
}

/* ── Thread stacks ───────────────────────────────────────────────── */

K_THREAD_STACK_DEFINE(thread_a_stack, STACK_SIZE);
K_THREAD_STACK_DEFINE(thread_b_stack, STACK_SIZE);

static struct k_thread thread_a_data;
static struct k_thread thread_b_data;

/* ── App entry (called from Zephyr's bg_thread_main) ──────────────── */

int zephyr_app_main(void)
{
    sim_trace_u32("zephyr_main", 1);

    /* Create threads. */
    k_thread_create(&thread_a_data, thread_a_stack,
                    K_THREAD_STACK_SIZEOF(thread_a_stack),
                    thread_a_entry,
                    NULL, NULL, NULL,
                    PRIO_A, 0, K_NO_WAIT);

    k_thread_create(&thread_b_data, thread_b_stack,
                    K_THREAD_STACK_SIZEOF(thread_b_stack),
                    thread_b_entry,
                    NULL, NULL, NULL,
                    PRIO_B, 0, K_NO_WAIT);

    sim_trace_u32("zephyr_main_done", 1);

    return 0;
}
