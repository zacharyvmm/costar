/*
 * Simulator hooks — sim_hooks.c
 *
 * This file provides the C-side implementation of simulator hooks.
 *
 * When compiled with -finstrument-functions (opt-in via
 * SIM_INSTRUMENT_FUNCTIONS=1), the __cyg_profile_func_enter and
 * __cyg_profile_func_exit hooks are defined here.  Every C function
 * entry calls sim_budget_poll(), which increments a budget counter
 * and yields the fiber if the budget is exceeded.  This prevents
 * cooperative-fiber infinite-loop stalls.
 *
 * When instrumentation is NOT enabled, the hooks are defined as
 * weak no-ops (the linker ignores them since the compiler doesn't
 * emit calls to them).
 */

#include "sim_abi.h"

/* ── Function-entry instrumentation (Tier 1 budget) ──────────────── */

#if defined(__GNUC__) || defined(__clang__)

/*
 * __cyg_profile_func_enter is called by GCC/Clang at every function
 * entry when -finstrument-functions is enabled.
 *
 * We use a weak definition so that firmware code can override it
 * if needed (e.g., to filter which functions trigger budget checks).
 */
__attribute__((weak))
void __cyg_profile_func_enter(void *this_fn, void *call_site)
{
    (void)this_fn;
    (void)call_site;
    sim_budget_poll();
}

/*
 * __cyg_profile_func_exit is called at every function return.
 * For the MVP, it's a no-op — we only check the budget on entry.
 */
__attribute__((weak))
void __cyg_profile_func_exit(void *this_fn, void *call_site)
{
    (void)this_fn;
    (void)call_site;
}

#endif /* __GNUC__ || __clang__ */
