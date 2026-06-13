/* app_main.c — Zephyr multi-threaded test app with peripheral demo.
 *
 * Creates two threads that yield/sleep cooperatively, AND demonstrates
 * the RTOS-agnostic peripheral event queue:
 *
 *   - A virtual timer is armed via sim_schedule_event().
 *   - When it fires (10,000 cycles = 10ms from now), the callback
 *     records a trace event and raises IRQ 5.
 *   - The drain loop dispatches the callback at the correct virtual
 *     time, interleaved with Zephyr thread execution.
 */

#include <zephyr/kernel.h>
#include "sim_abi.h"

/* Stack size for each thread. */
#define STACK_SIZE 1024
#define PRIO_A 5
#define PRIO_B 5

/* ── Peripheral: virtual timer callback ─────────────────────────── */

static void vtimer_callback(void)
{
    sim_trace_u32("vtimer_fired", 1);
    /* Raise IRQ 5 — in a full implementation, Zephyr's ISR would
       handle this.  For now we just record the trace event. */
    sim_irq_raise(5);
    sim_trace_u32("vtimer_irq_raised", 5);
}

/* ── Thread A ────────────────────────────────────────────────────── */

static void thread_a_entry(void *a, void *b, void *c)
{
    ARG_UNUSED(a); ARG_UNUSED(b); ARG_UNUSED(c);

    sim_trace_u32("thread_a_start", 1);

    /* Arm a virtual timer to fire at current time + 10,000 cycles.
       This will happen while thread_a is sleeping, demonstrating
       that peripheral events are dispatched at the right virtual
       time regardless of what the RTOS threads are doing. */
    sim_schedule_event(sim_now_ticks() + 10000, vtimer_callback);

    /* Sleep for 50,000 cycles (50ms at 1MHz).  The timer will fire
       at +10,000, well before we wake up. */
    k_sleep(K_USEC(50000));

    sim_trace_u32("thread_a_wake", 1);
}

/* ── Thread B ────────────────────────────────────────────────────── */

static void thread_b_entry(void *a, void *b, void *c)
{
    ARG_UNUSED(a); ARG_UNUSED(b); ARG_UNUSED(c);

    sim_trace_u32("thread_b_start", 1);
    k_yield();
    sim_trace_u32("thread_b_done", 1);
}

/* ── Thread stacks ───────────────────────────────────────────────── */

K_THREAD_STACK_DEFINE(thread_a_stack, STACK_SIZE);
K_THREAD_STACK_DEFINE(thread_b_stack, STACK_SIZE);
static struct k_thread thread_a_data;
static struct k_thread thread_b_data;

/* ── App entry ──────────────────────────────────────────────────── */

int zephyr_app_main(void)
{
    sim_trace_u32("zephyr_main", 1);

    k_thread_create(&thread_a_data, thread_a_stack,
                    K_THREAD_STACK_SIZEOF(thread_a_stack),
                    thread_a_entry, NULL, NULL, NULL,
                    PRIO_A, 0, K_NO_WAIT);

    k_thread_create(&thread_b_data, thread_b_stack,
                    K_THREAD_STACK_SIZEOF(thread_b_stack),
                    thread_b_entry, NULL, NULL, NULL,
                    PRIO_B, 0, K_NO_WAIT);

    sim_trace_u32("zephyr_main_done", 1);
    return 0;
}
