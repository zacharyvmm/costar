/*
 * nsi_shim.c — Thin C shim for nsi_* functions that need va_list handling.
 *
 * Compiled into sim-zephyr-port and linked alongside the Rust runner shim.
 * Provides proper implementations of nsi_vprint_* that Rust can't easily
 * handle due to C variadic ABI.
 */

#include <stdio.h>
#include <stdlib.h>
#include <stdarg.h>
#include <stdint.h>

/* ── nsi_vprint_* ──────────────────────────────────────────────── */

void nsi_vprint_trace(const char *format, va_list vargs)
{
    /* Route Zephyr's console/printf output to stdout. */
    vfprintf(stdout, format, vargs);
    fflush(stdout);
}

void nsi_vprint_warning(const char *format, va_list vargs)
{
    fprintf(stderr, "Zephyr WARNING: ");
    vfprintf(stderr, format, vargs);
    fprintf(stderr, "\n");
}

void nsi_vprint_error_and_exit(const char *format, va_list vargs)
{
    fprintf(stderr, "Zephyr ERROR: ");
    vfprintf(stderr, format, vargs);
    fprintf(stderr, "\n");
    fflush(stderr);
    exit(0);
}

/* ── nsi_trace_over_tty ────────────────────────────────────────── */

int nsi_trace_over_tty(int file_number)
{
    (void)file_number;
    return 0;
}

/* ── nsi_add_command_line_opts (no-op) ─────────────────────────── */

void nsi_add_command_line_opts(void) {}

/* ── nsi_simu_time ─────────────────────────────────────────────── */

/*
 * Global virtual time in nanoseconds. Zephyr's native_sim architecture
 * reads this to get the current simulated time.
 */
uint64_t nsi_simu_time;

/* ── nsi_hws_get_time ──────────────────────────────────────────── */

uint64_t nsi_hws_get_time(void)
{
    return nsi_simu_time;
}

/* ── nsi_exit ──────────────────────────────────────────────────── */

void nsi_exit(int exit_code)
{
    exit(exit_code);
}

/* ── nsi_get_cmd_line_args ─────────────────────────────────────── */

void *nsi_get_cmd_line_args(void) { return NULL; }
void *nsi_get_test_cmd_line_args(void) { return NULL; }
