/*
 * Edge-level instrumentation hooks (Tier 3)
 *
 * When compiled with -fsanitize-coverage=trace-pc-guard (opt-in via
 * SIM_INSTRUMENT_EDGES=1), Clang inserts calls to
 * __sanitizer_cov_trace_pc_guard at every basic-block edge.  This file
 * implements those callbacks with a fast throttle so that sim_budget_poll
 * is called every EDGE_CHECK_INTERVAL edges, giving the scheduler a
 * chance to preempt tight while(1){} loops that never call another
 * function.
 *
 * Without -fsanitize-coverage, the compiler does NOT emit calls to these
 * hooks, and the functions here become dead code (never linked unless
 * something else references them).
 *
 * ## Relationship to Tier 1 and Tier 2
 *
 *   Tier 1: -finstrument-functions → __cyg_profile_func_enter →
 *           sim_budget_poll  (function granularity)
 *   Tier 2: SIM_LOOP_POLL() → sim_budget_poll  (manual placement)
 *   Tier 3: -fsanitize-coverage → __sanitizer_cov_trace_pc_guard →
 *           [throttle] → sim_budget_poll  (edge granularity)
 *
 * All three tiers feed into the same sim_budget_poll / BudgetState
 * mechanism.  Tier 3 is the only tier that can intercept tight
 * while(1){} loops that contain no function calls and no manual
 * SIM_LOOP_POLL() placements.
 *
 * ## Compiler support
 *
 *   Clang: fully supported (-fsanitize-coverage=trace-pc-guard)
 *   GCC:   -fsanitize-coverage=trace-pc is available, but guard
 *          variant semantics differ; Tier 3 primarily targets Clang.
 *   MSVC:  not supported (would require /Gh or /GH with different ABI).
 *
 * ## Performance
 *
 * The edge counter is a fast __thread variable.  With
 * EDGE_CHECK_INTERVAL=10000, we call sim_budget_poll roughly once per
 * 10K edges, which adds <0.1% overhead on typical workloads.
 */

#include "sim_abi.h"
#include <stdint.h>

/* ── Edge-check interval ──────────────────────────────────────────────
 *
 * How many basic-block edges between sim_budget_poll calls.
 *
 * Lower values give finer-grained preemption but higher overhead.
 * 10 000 edges at ~2 ns per edge ≈ 20 µs between checks, which is
 * fast enough to feel "instant" while adding negligible overhead.
 *
 * Override at compile time with -DSIM_EDGE_CHECK_INTERVAL=<N>.
 */
#ifndef SIM_EDGE_CHECK_INTERVAL
#define SIM_EDGE_CHECK_INTERVAL 10000
#endif

/* ── Edge counter ─────────────────────────────────────────────────────
 *
 * Thread-local so it's fast and single-thread-safe.  The simulator
 * is single-threaded, so no real thread safety is needed — __thread
 * is purely a performance mechanism to avoid atomic operations.
 *
 * The counter persists across fiber switches (all fibers share the
 * same host thread).  This means the first budget check for a newly-
 * resumed task may occur slightly sooner or later than the configured
 * interval, depending on where the previous task left the counter.
 * The budget state (BudgetState in sim-ffi) provides the actual
 * preemption limit; the edge counter is only a throttle.
 */
static __thread uint64_t sim_edge_counter = 0;

/* ── Coverage guard callbacks ─────────────────────────────────────── */

/*
 * Called by Clang at every basic-block edge when
 * -fsanitize-coverage=trace-pc-guard is enabled.
 *
 * Each edge gets its own 4-byte guard variable (initially zero).
 * We set it to 1 as a side-effect (marking the edge as covered);
 * the meaningful action is the budget check.
 *
 * Clang guarantees this function is NOT itself instrumented, so
 * there is no infinite recursion risk.
 *
 * This function runs on the fiber's stack and can safely call
 * sim_budget_poll, which may suspend the fiber if the budget is
 * exceeded.
 */
void __sanitizer_cov_trace_pc_guard(uint32_t *guard)
{
    *guard = 1;

    sim_edge_counter++;
    if (sim_edge_counter >= SIM_EDGE_CHECK_INTERVAL) {
        sim_edge_counter = 0;
        /* NULL/0 → synthetic call site (generated edge code) */
        sim_budget_poll(NULL, 0);
    }
}

/*
 * Called once at startup with the range of guard variables.
 * For our purposes, this is a no-op — we do not need to track
 * which edges were hit, only that we can intercept them for
 * budget checks.
 */
void __sanitizer_cov_trace_pc_guard_init(uint32_t *start, uint32_t *stop)
{
    (void)start;
    (void)stop;
}
