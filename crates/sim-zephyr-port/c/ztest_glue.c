/* ztest_glue.c — Non-inline wrappers for ztest static inline functions.
 *
 * ztest_test.h defines several functions as static inline.  At -O0,
 * the compiler doesn't emit the symbols, causing undefined references.
 * This file provides global non-inline versions.
 */

#include <stdint.h>
#include <stdbool.h>

/* ── ztest_run_test_suites ────────────────────────────────────────── */

extern int z_impl_ztest_run_test_suites(const void *state, bool shuffle,
                                        int suite_iter, int case_iter);

int ztest_run_test_suites(const void *state, bool shuffle,
                          int suite_iter, int case_iter)
{
    return z_impl_ztest_run_test_suites(state, shuffle, suite_iter, case_iter);
}

/* ── __ztest_set_test_result / __ztest_set_test_phase ─────────────── */
/* These are defined in ztest.c but accessed via static inline wrappers
   in ztest_test.h. */

enum ztest_result { ZTEST_RESULT_PASS = 0, ZTEST_RESULT_FAIL = 1, ZTEST_RESULT_SKIP = 2 };
enum ztest_phase { ZTEST_PHASE_SETUP = 0, ZTEST_PHASE_TEST = 1, ZTEST_PHASE_TEARDOWN = 2 };

extern void z_impl___ztest_set_test_result(enum ztest_result new_result);
extern void z_impl___ztest_set_test_phase(enum ztest_phase new_phase);

void __ztest_set_test_result(enum ztest_result new_result)
{
    z_impl___ztest_set_test_result(new_result);
}

void __ztest_set_test_phase(enum ztest_phase new_phase)
{
    z_impl___ztest_set_test_phase(new_phase);
}
