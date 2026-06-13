/* app_broader_api.c — Zephyr broader RTOS API exercises.
 *
 * Tests k_sem, k_mutex, k_msgq, k_timer, and k_work via trace events
 * in the real Zephyr kernel (cc crate build).
 */

#include <zephyr/kernel.h>
#include "sim_abi.h"

static K_SEM_DEFINE(test_sem, 0, 1);
static K_MUTEX_DEFINE(test_mutex);
K_MSGQ_DEFINE(test_msgq, sizeof(uint32_t), 4, 4);
static struct k_timer test_timer;
static struct k_work  test_work;

#define STACK_SIZE 1024
K_THREAD_STACK_DEFINE(consumer_stack, STACK_SIZE);
static struct k_thread consumer_thread_data;

static void timer_callback(struct k_timer *timer) { ARG_UNUSED(timer); sim_trace_u32("timer_fired", 1); }
static void work_handler(struct k_work *work)     { ARG_UNUSED(work);  sim_trace_u32("work_executed", 1); }

static void consumer_entry(void *a, void *b, void *c)
{
    ARG_UNUSED(a); ARG_UNUSED(b); ARG_UNUSED(c);
    sim_trace_u32("consumer_wait_sem", 1);
    k_sem_take(&test_sem, K_FOREVER);
    sim_trace_u32("consumer_got_sem", 1);
    sim_trace_u32("consumer_lock_mutex", 1);
    k_mutex_lock(&test_mutex, K_FOREVER);
    sim_trace_u32("consumer_got_mutex", 1);
    k_mutex_unlock(&test_mutex);
    sim_trace_u32("consumer_unlock_mutex", 1);
    sim_trace_u32("consumer_recv_msgq", 1);
    uint32_t msg;
    k_msgq_get(&test_msgq, &msg, K_FOREVER);
    sim_trace_u32("consumer_got_msg", msg);
    sim_trace_u32("consumer_done", 1);
}

int zephyr_app_main(void)
{
    sim_trace_u32("zephyr_main_start", 1);
    k_timer_init(&test_timer, timer_callback, NULL);
    k_work_init(&test_work, work_handler);
    k_thread_create(&consumer_thread_data, consumer_stack,
                    K_THREAD_STACK_SIZEOF(consumer_stack),
                    consumer_entry, NULL, NULL, NULL, 0, 0, K_NO_WAIT);

    /* ── Semaphore ──────────────────────────────────────────── */
    sim_trace_u32("producer_give_sem", 1);
    k_sem_give(&test_sem);
    k_yield();

    /* ── Mutex ──────────────────────────────────────────────── */
    sim_trace_u32("producer_lock_mutex", 1);
    k_mutex_lock(&test_mutex, K_FOREVER);
    sim_trace_u32("producer_got_mutex", 1);
    k_mutex_unlock(&test_mutex);
    sim_trace_u32("producer_unlock_mutex", 1);

    /* ── Msgq ───────────────────────────────────────────────── */
    sim_trace_u32("producer_send_msgq", 1);
    uint32_t msg = 42;
    k_msgq_put(&test_msgq, &msg, K_FOREVER);
    k_yield();

    /* ── Timer (one-shot, 20ms) ─────────────────────────────── */
    sim_trace_u32("producer_start_timer", 1);
    k_timer_start(&test_timer, K_MSEC(20), K_NO_WAIT);

    /* ── Workqueue ──────────────────────────────────────────── */
    sim_trace_u32("producer_submit_work", 1);
    k_work_submit(&test_work);

    /* Sleep for 50ms to let timer fire and workqueue run. */
    k_sleep(K_MSEC(50));

    k_timer_stop(&test_timer);
    sim_trace_u32("zephyr_main_done", 1);
    return 0;
}
