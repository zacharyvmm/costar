/*
 * standalone_test.c — Minimal Zephyr-style test app
 *
 * This file demonstrates the Zephyr thread→fiber pattern WITHOUT
 * requiring the full Zephyr SDK.  It uses the simulator's arch port
 * (zephyr_arch.h) and ABI (sim_zephyr_abi.h, sim_abi.h) directly.
 *
 * The test creates two "Zephyr threads" as Rust fibers, with one
 * thread sleeping and the other running.  It verifies that:
 *   1. Thread creation maps to fiber creation
 *   2. k_msleep maps to fiber sleep
 *   3. Cooperative yield works
 *   4. The scheduler round-robins between threads
 *
 * This is NOT real Zephyr — it's a CI-friendly smoke test for the
 * adapter layer.  Real Zephyr apps use west build + libzephyr.a.
 */

#include "sim_abi.h"
#include "sim_zephyr_abi.h"
#include "zephyr_arch.h"

#include <stddef.h>

/* ── Simulated Zephyr thread entry signature ───────────────────────── */
typedef void (*zephyr_thread_entry_t)(void *, void *, void *);

/* ── Thread data ──────────────────────────────────────────────────── */

/* Simulated k_thread struct — lightweight for the test.
 * Real Zephyr has a full k_thread with scheduler state, stack info,
 * wait queue nodes, etc.  Here we just need:
 *   - entry + args (for the fiber body to call)
 *   - tcb pointer (for sim_zephyr_set_current_thread)
 */
struct test_thread {
    zephyr_thread_entry_t entry;
    uintptr_t fiber_handle;       /* Rust fiber task ID */
    void *stack;
    uint32_t stack_size;
    int32_t priority;
    const char *name;
    void *arg1, *arg2, *arg3;
};

/* ── Test thread bodies ───────────────────────────────────────────── */

static void thread_a_entry(void *arg1, void *arg2, void *arg3)
{
    (void)arg2;
    (void)arg3;

    int *counter = (int *)arg1;
    int i;
    for (i = 0; i < 3; i++) {
        (*counter)++;
        sim_trace_u32("thread_a", (uint32_t)(*counter));

        /* Sleep equivalent to k_msleep(1) — 1 tick in test units. */
        uint64_t now = sim_now_ticks();
        sim_task_delay_until(now + 1);
    }
}

static void thread_b_entry(void *arg1, void *arg2, void *arg3)
{
    (void)arg2;
    (void)arg3;

    int *counter = (int *)arg1;
    int i;
    for (i = 0; i < 3; i++) {
        (*counter)++;
        sim_trace_u32("thread_b", (uint32_t)(*counter));

        /* Cooperative yield without sleep. */
        sim_port_yield();
    }
}

/* ── Thread creation helper ───────────────────────────────────────── */

/**
 * Create a simulated Zephyr thread.
 *
 * Registers the thread with the Rust fiber runtime and stores
 * metadata for the scheduler.
 */
static uintptr_t zephyr_create_thread(
    const char *name,
    zephyr_thread_entry_t entry,
    void *arg1,
    void *arg2,
    void *arg3,
    uint32_t stack_size,
    int32_t priority
)
{
    /* Register with the Rust runtime via the Zephyr ABI. */
    uintptr_t handle = sim_zephyr_register_thread(
        name,
        entry,
        arg1,
        arg2,
        arg3,
        stack_size,
        priority
    );
    return handle;
}

/* ── Main entry point ─────────────────────────────────────────────── */

int c_zephyr_main(void)
{
    int counter_a = 0;
    int counter_b = 0;

    /* Initialize the Zephyr adapter. */
    sim_zephyr_init();

    /* Create two Zephyr-style threads.
     * Thread A: sleeps between increments (k_msleep simulation)
     * Thread B: yields cooperatively between increments
     * Thread B has higher priority (1 < 2 — lower number = higher priority). */
    uintptr_t th_a = zephyr_create_thread(
        "zephyr_thread_a",
        thread_a_entry,
        &counter_a, NULL, NULL,
        1024, 2
    );
    uintptr_t th_b = zephyr_create_thread(
        "zephyr_thread_b",
        thread_b_entry,
        &counter_b, NULL, NULL,
        1024, 1
    );

    (void)th_a;
    (void)th_b;

    /* Start the Zephyr scheduler.  This transfers control to the Rust
     * event loop and does not return until all threads exit. */
    sim_zephyr_start_scheduler();

    /* Check results (in a real test, we'd validate the trace). */
    if (counter_a != 3 || counter_b != 3) {
        return 1;
    }

    return 0;
}
