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
#include <unistd.h>

/* Rust function: flush pending trace events before _exit. */
void flush_trace_pending(void);

/* ── nsi_vprint_* ──────────────────────────────────────────────── */

void nsi_vprint_trace(const char *format, va_list vargs)
{
    /* Route Zephyr's console/printf output to stdout (unbuffered). */
    vfprintf(stdout, format, vargs);
    fflush(stdout);
}

void nsi_vprint_warning(const char *format, va_list vargs)
{
    fprintf(stderr, "Zephyr WARNING: ");
    vfprintf(stderr, format, vargs);
    fprintf(stderr, "\n");
    fflush(stderr);
}

void nsi_vprint_error_and_exit(const char *format, va_list vargs)
{
    fprintf(stderr, "Zephyr ERROR: ");
    vfprintf(stderr, format, vargs);
    fprintf(stderr, "\n");
    fflush(stderr);
    /* Flush pending trace events before exiting. */
    flush_trace_pending();
    _exit(0);
}

/* ── nsi_trace_over_tty ────────────────────────────────────────── */

int nsi_trace_over_tty(int file_number)
{
    (void)file_number;
    return 0;
}

struct args_struct_t;

/* ── nsi_add_command_line_opts (no-op) ─────────────────────────── */

void nsi_add_command_line_opts(struct args_struct_t *args)
{
    (void)args;
}

/* ── nsi_simu_time ─────────────────────────────────────────────── */

uint64_t nsi_simu_time;

/* ── nsi_hws_get_time ──────────────────────────────────────────── */

uint64_t nsi_hws_get_time(void)
{
    return nsi_simu_time;
}

/* ── nsi_exit ──────────────────────────────────────────────────── */

void nsi_exit(int exit_code)
{
    flush_trace_pending();
    _exit(exit_code);
}

/* Register an atexit handler so direct exit() calls also flush. */
__attribute__((constructor))
static void _nsi_register_atexit(void)
{
    atexit(flush_trace_pending);
}

/* ── nsi_get_cmd_line_args ─────────────────────────────────────── */

void nsi_get_cmd_line_args(int *argc, char ***argv)
{
    if (argc != NULL) {
        *argc = 0;
    }
    if (argv != NULL) {
        *argv = NULL;
    }
}

void nsi_get_test_cmd_line_args(int *argc, char ***argv)
{
    nsi_get_cmd_line_args(argc, argv);
}

void nsi_hws_init(void) {}
void nsi_hws_cleanup(void) {}
void nsi_hws_one_event(void) {}
void nsi_hws_set_end_of_time(uint64_t new_end_of_time)
{
    (void)new_end_of_time;
}
void nsi_hws_find_next_event(void) {}
uint64_t nsi_hws_get_next_event_time(void)
{
    return UINT64_MAX;
}

/* Force stdout to be unbuffered from the start. */
__attribute__((constructor))
static void _nsi_init_stdout(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
}
