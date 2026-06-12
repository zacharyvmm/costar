/*
 * Host stubs for the cc-based Zephyr kernel build on non-ELF hosts.
 *
 * The native_sim console and timer drivers depend on Zephyr's normal linker
 * script and POSIX-oriented host setup. The costar runner boots Zephyr
 * explicitly and drives virtual time itself, so we provide the small runtime
 * surface those drivers would otherwise export.
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>

extern uint64_t nsi_simu_time;

int arch_printk_char_out(int c)
{
    fputc(c, stdout);
    if (c == '\n' || c == '\r') {
        fflush(stdout);
    }
    return c;
}

void posix_flush_stdout(void)
{
    fflush(stdout);
}

uint32_t sys_clock_cycle_get_32(void)
{
    return (uint32_t)nsi_simu_time;
}

uint64_t sys_clock_cycle_get_64(void)
{
    return nsi_simu_time;
}

void sys_clock_set_timeout(int32_t ticks, bool idle)
{
    (void)ticks;
    (void)idle;
}

uint32_t sys_clock_elapsed(void)
{
    return 0;
}

void sys_clock_idle_exit(void) {}
void sys_clock_disable(void) {}
