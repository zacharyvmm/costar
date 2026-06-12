// standalone_test.c — Zephyr hello-thread demo (no Zephyr SDK needed).
//
// This file implements a minimal Zephyr-like API for the standalone
// simulator test.  It provides:
//   - zephyr_thread_spawn() — create and start a thread
//   - zephyr_sleep() — sleep for N ticks
//
// The actual thread lifecycle and scheduling are handled by the Rust
// fiber runtime via sim_zephyr_abi.h and sim_abi.h.

#include "sim_zephyr_abi.h"
#include "sim_abi.h"

// ── Thread 1: Blinker ─────────────────────────────────────────────
//
// Higher priority (5).  Traces "zephyr_hello", sleeps 2 ticks,
// traces "zephyr_wake", then exits.

static void blinker_entry(void *a, void *b, void *c)
{
    (void)a; (void)b; (void)c;

    sim_trace_u32("zephyr_hello", 1);

    zephyr_sleep(2);

    sim_trace_u32("zephyr_wake", 1);
}

// ── Thread 2: Worker ──────────────────────────────────────────────
//
// Lower priority (3).  Traces "zephyr_worker_start", sleeps 1 tick,
// traces "zephyr_worker_done", then exits.

static void worker_entry(void *a, void *b, void *c)
{
    (void)a; (void)b; (void)c;

    sim_trace_u32("zephyr_worker_start", 1);

    zephyr_sleep(1);

    sim_trace_u32("zephyr_worker_done", 1);
}

// ── Zephyr entry point ────────────────────────────────────────────

int c_zephyr_main(void)
{
    // Initialize the Zephyr adapter (thread registry, scheduler state).
    sim_zephyr_init();

    // Spawn threads.
    //
    // blinker: priority 5 — higher than worker's 3, so it runs first.
    // worker:  priority 3 — runs when blinker sleeps.
    zephyr_thread_spawn("blinker", blinker_entry, NULL, NULL, NULL, 1024, 5);
    zephyr_thread_spawn("worker", worker_entry, NULL, NULL, NULL, 1024, 3);

    // Start the Rust Zephyr scheduler.  Does not return.
    sim_zephyr_start_scheduler();

    return 0;
}
