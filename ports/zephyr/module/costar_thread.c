/*
 * The running thread's declared stack size, for costar's native_sim runner
 * (ports/zephyr/runner.c).  The runner only sees Zephyr's threads through
 * the nct_* interface, which carries no stack size; this file is built
 * inside the Zephyr build, so it can read the thread itself.
 */
#include <stddef.h>

#include <zephyr/kernel.h>
#include <zephyr/kernel_structs.h>

#include <nsi_cpu_if.h>
#include <posix_core.h>

/* NATIVE_SIMULATOR_IF: the embedded image is linked with --gc-sections, and
 * this is how native_sim keeps what its runner calls. */
NATIVE_SIMULATOR_IF size_t costar_zephyr_thread_stack_size(void)
{
	/* Not k_current_get(): the main thread starts before the kernel counts
	 * as booted. */
	const struct k_thread *thread = _current;

#ifdef CONFIG_ARCH_POSIX_UPDATE_STACK_INFO
	/* stack_info now describes the host stack; Zephyr kept its own. */
	const posix_thread_status_t *status = thread->callee_saved.thread_status;

	return status->zephyr_stack_size;
#else
	return thread->stack_info.size;
#endif
}
