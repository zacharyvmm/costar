/* standalone_broader_api.c — Broader API exercises for the standalone test.
 *
 * Tests semaphore-like, mutex-like, and msgq-like primitives using
 * only the standalone test primitives:
 *   - zephyr_thread_spawn(name, entry, ...)
 *   - zephyr_sleep(ticks)
 *   - sim_trace_u32
 *
 * All threads have the same priority so round-robin scheduling
 * applies.  The producer and consumer ping-pong via sem/mutex/msgq.
 */

#include "sim_zephyr_abi.h"
#include "sim_abi.h"
#include <stdint.h>

/* ── Simulated kernel objects ─────────────────────────────────────── */

static volatile int g_sem_count = 0;

static void sem_give(void) { g_sem_count++; }
static void sem_take(void) {
    while (g_sem_count == 0) zephyr_sleep(1);
    g_sem_count--;
}

static volatile int g_mutex_locked = 0;

static void mutex_lock(void) {
    while (g_mutex_locked) zephyr_sleep(1);
    g_mutex_locked = 1;
}
static void mutex_unlock(void) { g_mutex_locked = 0; }

static volatile uint32_t g_msgq_buf[4];
static volatile int g_msgq_head, g_msgq_tail, g_msgq_count;

static void msgq_put(uint32_t val) {
    while (g_msgq_count >= 4) zephyr_sleep(1);
    g_msgq_buf[g_msgq_tail] = val;
    g_msgq_tail = (g_msgq_tail + 1) % 4;
    g_msgq_count++;
}
static uint32_t msgq_get(void) {
    uint32_t val;
    while (g_msgq_count == 0) zephyr_sleep(1);
    val = g_msgq_buf[g_msgq_head];
    g_msgq_head = (g_msgq_head + 1) % 4;
    g_msgq_count--;
    return val;
}

/* ── Consumer ────────────────────────────────────────────────────── */
static void consumer_entry(void *a, void *b, void *c) {
    (void)a;(void)b;(void)c;
    sim_trace_u32("consumer_wait_sem", 1);
    sem_take();
    sim_trace_u32("consumer_got_sem", 1);
    sim_trace_u32("consumer_lock_mutex", 1);
    mutex_lock();
    sim_trace_u32("consumer_got_mutex", 1);
    mutex_unlock();
    sim_trace_u32("consumer_unlock_mutex", 1);
    sim_trace_u32("consumer_recv_msgq", 1);
    uint32_t msg = msgq_get();
    sim_trace_u32("consumer_got_msg", msg);
    sim_trace_u32("consumer_done", 1);
}

/* ── Producer ─────────────────────────────────────────────────────── */
static void producer_entry(void *a, void *b, void *c) {
    (void)a;(void)b;(void)c;
    sim_trace_u32("producer_give_sem", 1);
    sem_give();
    zephyr_sleep(1);
    sim_trace_u32("producer_lock_mutex", 1);
    mutex_lock();
    sim_trace_u32("producer_got_mutex", 1);
    mutex_unlock();
    sim_trace_u32("producer_unlock_mutex", 1);
    sim_trace_u32("producer_send_msgq", 1);
    msgq_put(42);
    zephyr_sleep(1);
    sim_trace_u32("producer_done", 1);
}

/* ── Entry point ──────────────────────────────────────────────────── */
int cz_broader_api_main(void) {
    sim_trace_u32("broader_api_start", 1);
    sim_zephyr_init();
    zephyr_thread_spawn("producer", producer_entry, NULL, NULL, NULL, 1024, 5);
    zephyr_thread_spawn("consumer", consumer_entry, NULL, NULL, NULL, 1024, 5);
    sim_zephyr_start_scheduler();
    return 0;
}
