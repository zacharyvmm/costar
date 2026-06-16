/*
 * sim_arch.c — Minimal arch layer overrides for costar Zephyr cc build.
 *
 * Provides ONLY the functions that need custom corosensei behavior.
 * All other posix_/arch_ functions come from the original Zephyr
 * board/soc/arch sources (soc.c, irq_handler.c, swap.c, thread.c, etc.)
 * which are compiled alongside this file.
 */

#include <zephyr/kernel.h>
#include <zephyr/kernel_structs.h>
#include <zephyr/sys_clock.h>
#include <zephyr/init.h>
#include <stdint.h>
#include "posix_core.h"
#include "nct_if.h"
#include "kswap.h"
#include "ksched.h"
#include "sim_abi.h"

/* ── From sim-ffi (Rust ABI) ─────────────────────────────────────── */
extern void sim_enter_critical(void);
extern void sim_exit_critical(void);

/* ── From nsi_shim.c ─────────────────────────────────────────────── */
extern void nsi_exit(int exit_code);

/* ── Thread entry (called by posix_arch_init) ────────────────────── */
extern void posix_arch_thread_entry(void *pa_thread_status);

/* ── Global state ────────────────────────────────────────────────── */
static void *te_state;

/* ══════════════════════════════════════════════════════════════════
 * POSIX ARCH INIT / CLEANUP — overrides soc.c
 * ══════════════════════════════════════════════════════════════════ */

void posix_arch_init(void)
{
	te_state = nct_init(posix_arch_thread_entry);
}

void posix_arch_clean_up(void)
{
	nct_clean_up(te_state);
}

/* ══════════════════════════════════════════════════════════════════
 * THREAD CREATION — overrides thread.c
 * ══════════════════════════════════════════════════════════════════ */

void arch_new_thread(struct k_thread *thread, k_thread_stack_t *stack,
		     char *stack_ptr, k_thread_entry_t entry,
		     void *p1, void *p2, void *p3)
{
	posix_thread_status_t *thread_status =
		Z_STACK_PTR_TO_FRAME(posix_thread_status_t, stack_ptr);

	thread_status->entry_point = entry;
	thread_status->arg1 = p1;
	thread_status->arg2 = p2;
	thread_status->arg3 = p3;
	thread_status->thread_idx =
		posix_new_thread((void *)thread_status);

	thread->callee_saved.thread_status = (void *)thread_status;
}

/* ══════════════════════════════════════════════════════════════════
 * THREAD SWITCHING — overrides swap.c
 * ══════════════════════════════════════════════════════════════════ */

int arch_swap(unsigned int key)
{
	_current->callee_saved.key = key;
	_current->callee_saved.retval = -EAGAIN;

	posix_thread_status_t *ready_thread_ptr =
		(posix_thread_status_t *)
		_kernel.ready_q.cache->callee_saved.thread_status;

	posix_thread_status_t *this_thread_ptr =
		(posix_thread_status_t *)
		_current->callee_saved.thread_status;

	z_current_thread_set(_kernel.ready_q.cache);

	posix_swap(ready_thread_ptr->thread_idx,
		   this_thread_ptr->thread_idx);

	irq_unlock(_current->callee_saved.key);

	return _current->callee_saved.retval;
}

void arch_switch_to_main_thread(struct k_thread *main_thread,
				char *stack_ptr, k_thread_entry_t _main)
{
	ARG_UNUSED(main_thread);
	ARG_UNUSED(stack_ptr);
	ARG_UNUSED(_main);

	posix_thread_status_t *ready_thread_ptr =
		(posix_thread_status_t *)
		_kernel.ready_q.cache->callee_saved.thread_status;

	z_current_thread_set(_kernel.ready_q.cache);

	posix_main_thread_start(ready_thread_ptr->thread_idx);
}

/* ══════════════════════════════════════════════════════════════════
 * POSIX CORE WRAPPERS — overrides posix_core_nsi.c
 * Maps posix_* to nct_* (corosensei fiber yield instead of pthreads).
 * ══════════════════════════════════════════════════════════════════ */

void posix_swap(int next, int this)
{
	(void)this;
	nct_swap_threads(te_state, next);
}

int posix_new_thread(void *payload)
{
	return nct_new_thread(te_state, payload);
}

void posix_main_thread_start(int next)
{
	nct_first_thread_start(te_state, next);
}

void posix_abort_thread(int thread_idx)
{
	nct_abort_thread(te_state, thread_idx);
}

int posix_arch_get_unique_thread_id(int thread_idx)
{
	return nct_get_unique_thread_id(te_state, thread_idx);
}

int posix_arch_thread_name_set(int thread_idx, const char *str)
{
	return nct_thread_name_set(te_state, thread_idx, str);
}

void posix_arch_thread_entry(void *pa_thread_status)
{
	posix_thread_status_t *ptr = (posix_thread_status_t *)pa_thread_status;
	posix_irq_full_unlock();
		z_thread_entry(ptr->entry_point, ptr->arg1, ptr->arg2, ptr->arg3);
	}

	/* ══════════════════════════════════════════════════════════════════
	 * THREAD ABORT — arch must provide z_impl_k_thread_abort when
	 * CONFIG_ARCH_HAS_THREAD_ABORT=y.  Calls Zephyr's internal
	 * z_thread_abort() for cleanup, then triggers reschedule.
	 * ══════════════════════════════════════════════════════════════════ */

	#include <ksched.h>
	void z_thread_abort(k_tid_t thread);

	void z_impl_k_thread_abort(k_tid_t thread)
	{
		z_thread_abort(thread);
		unsigned int key = arch_irq_lock();
		z_reschedule_irqlock(key);
	}

	/* Stub for z_fatal_error — called by z_thread_halt if an essential
	   thread is aborted.  Our test apps don't use essential threads. */
	void z_fatal_error(unsigned int reason, const struct arch_esf *esf) {
		(void)reason; (void)esf;
	}

	/* ══════════════════════════════════════════════════════════════════
	 * TIMEOUT HOOK — intercepts sys_clock_set_timeout to record the
 * kernel's next wake deadline.  Called by Zephyr's timeout subsystem
 * whenever a new timeout is added to (or removed from) the queue.
 *
 * Stores the delta ticks until the next timeout (relative to now),
 * or INT64_MAX if no timeout is pending.  The Rust drain loop reads
 * this to decide how far to advance virtual time before calling
 * sys_clock_announce().
 * ══════════════════════════════════════════════════════════════════ */

volatile int64_t g_rtos_ticks_until_wake = INT64_MAX;

void sys_clock_set_timeout(int32_t ticks, bool idle)
{
	/* Ignore calls from the idle thread — it passes K_TICKS_FOREVER
	   and would overwrite a legitimate timeout set by a sleeping
	   application thread. */
	if (idle) {
		return;
	}

	if (ticks == K_TICKS_FOREVER || ticks <= 0 || ticks > 1000000) {
		g_rtos_ticks_until_wake = INT64_MAX;
	} else {
		g_rtos_ticks_until_wake = ticks;
	}
}

/* ── Timer driver stubs (replacing native_posix_timer.c) ────────── */

/* Extern: nsi_simu_time is defined in nsi_shim.c */
extern uint64_t nsi_simu_time;

uint32_t sys_clock_cycle_get_32(void)
{
	return (uint32_t)nsi_simu_time;
}

uint64_t sys_clock_cycle_get_64(void)
{
	return nsi_simu_time;
}

uint32_t sys_clock_elapsed(void)
{
	/* We control time externally; the kernel's tick accounting is
	   driven by z_clock_announce() calls from the Rust drain loop.
	   Report 0 here so the kernel doesn't try to self-advance. */
	return 0;
}

void sys_clock_disable(void)
{
	/* No-op: virtual time never stops. */
}

static int sys_clock_driver_init(void)
{
	return 0;
}

#ifndef __APPLE__
SYS_INIT(sys_clock_driver_init, PRE_KERNEL_2,
	 CONFIG_SYSTEM_CLOCK_INIT_PRIORITY);
#endif

/* ── Time advancement helper (called from Rust drain loop) ──────── */

/* Declare the kernel's tick advancement function (defined in
   kernel/timeout.c, not in a header we include). */
extern void sys_clock_announce(int32_t ticks);

void sim_clock_announce(int32_t ticks)
{
	sys_clock_announce(ticks);
}

/* ── Reschedule trigger for drain loop ────────────────────────────── */

#include <ksched.h>

/* Return the thread index (nct thread id) of the highest-priority
   ready thread, or -1 if none.  Called from the drain loop after
   sim_clock_announce to detect if a thread was woken. */
int sim_get_ready_thread_id(void)
{
	struct k_thread *next = z_swap_next_thread();
	if (next == NULL) {
		return -1;
	}
	return ((posix_thread_status_t *)next->callee_saved.thread_status)->thread_idx;
}

void sim_zephyr_reschedule(void)
{
	uint32_t key = arch_irq_lock();
	z_reschedule_irqlock(key);
}
