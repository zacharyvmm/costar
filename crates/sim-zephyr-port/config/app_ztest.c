/* app_ztest.c — Zephyr ztest integration demo.
 *
 * Uses Zephyr's ztest framework to verify k_sem, k_mutex, and k_msgq
 * APIs.  Test results are reported via the native_sim console output
 * (which goes through nsi_vprint_trace → stdout).
 *
 * Compile with CONFIG_ZTEST=y and ZEPHYR_APP=ztest.
 */

#include <zephyr/ztest.h>
#include "sim_abi.h"

/* ── Test objects ─────────────────────────────────────────────────── */

static K_SEM_DEFINE(test_sem, 0, 1);
static K_MUTEX_DEFINE(test_mutex);
K_MSGQ_DEFINE(test_msgq, sizeof(uint32_t), 4, 4);

/* ── Test cases ───────────────────────────────────────────────────── */

ZTEST(costar_suite, test_sem_give_take)
{
    sim_trace_u32("ztest_sem_start", 1);
    /* Give the semaphore (initialized to 0, so now it's available). */
    k_sem_give(&test_sem);
    /* Take it — should succeed immediately. */
    int ret = k_sem_take(&test_sem, K_NO_WAIT);
    zassert_equal(ret, 0, "k_sem_take should succeed after give");
    sim_trace_u32("ztest_sem_pass", 1);
}

ZTEST(costar_suite, test_mutex_lock_unlock)
{
    sim_trace_u32("ztest_mutex_start", 1);
    int ret = k_mutex_lock(&test_mutex, K_NO_WAIT);
    zassert_equal(ret, 0, "k_mutex_lock should succeed");
    ret = k_mutex_unlock(&test_mutex);
    zassert_equal(ret, 0, "k_mutex_unlock should succeed");
    sim_trace_u32("ztest_mutex_pass", 1);
}

ZTEST(costar_suite, test_msgq_put_get)
{
    sim_trace_u32("ztest_msgq_start", 1);
    uint32_t send = 42;
    k_msgq_put(&test_msgq, &send, K_NO_WAIT);
    uint32_t recv = 0;
    int ret = k_msgq_get(&test_msgq, &recv, K_NO_WAIT);
    zassert_equal(ret, 0, "k_msgq_get should succeed");
    zassert_equal(recv, 42, "received value should match sent value");
    sim_trace_u32("ztest_msgq_pass", 1);
}

/* ── Test suite registration ──────────────────────────────────────── */

ZTEST_SUITE(costar_suite, NULL, NULL, NULL, NULL, NULL);

/* ── Zephyr main (ztest entry) ────────────────────────────────────── */

/* ztest expects the test binary's main() to call ztest_run_all().
   We compile ztest.c with -Dmain=zephyr_ztest_main to avoid symbol
   collision with Rust's main().  Our entry point delegates to it. */

extern int zephyr_ztest_main(void);

int zephyr_app_main(void)
{
    sim_trace_u32("ztest_main_start", 1);
    /* zephyr_ztest_main runs all registered suites, reports results. */
    int result = zephyr_ztest_main();
    sim_trace_u32("ztest_main_done", (uint32_t)result);
    return result;
}
