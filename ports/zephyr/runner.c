/*
 * Zephyr native_sim runner for costar — runner.c
 *
 * Zephyr's native_sim board splits firmware in two parts:
 *   - the embedded image (kernel, application, board and arch code), built
 *     by an ordinary `west build -b native_sim/native/64`, and
 *   - a "native simulator runner" that provides threads (nct_*), CPU boot,
 *     halt and wake (nce_*), the timer model (hwtimer_*), the interrupt
 *     controller model (hw_irq_ctrl_*), and printing/exit (nsi_*).
 *
 * This file is that runner, implemented on costar's virtual CPU.  It is linked
 * into the image next to the unmodified embedded part; Zephyr itself is not
 * patched.  The interrupt-controller model follows the upstream one in
 * scripts/native_simulator/native/src/irq_ctrl.c.
 *
 * costar IRQ lines 0..31 are Zephyr's IRQs (the tick timer is IRQ 0).  The
 * busy-wait "wake me at" timer uses its own line and raises no Zephyr IRQ.
 */
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#include "costar.h"

/* What the embedded image provides (nsi_cpu_if.h). */
void nsif_cpu0_pre_cmdline_hooks(void);
void nsif_cpu0_pre_hw_init_hooks(void);
void nsif_cpu0_boot(void);
int nsif_cpu0_cleanup(void);
void nsif_cpu0_irq_raised_from_sw(void);

#define NSI_NEVER       UINT64_MAX
#define N_IRQS          32
#define TIMER_TICK_IRQ  0
#define WAKE_LINE       ( COSTAR_IRQ_LINES - 1 )

/* Simulated time in microseconds.  The embedded image reads it directly. */
uint64_t nsi_simu_time;

static void sync_time(void)
{
    nsi_simu_time = costar_now() / 1000;
}

static costar_time to_costar_time(uint64_t us)
{
    return us >= COSTAR_NEVER / 1000 ? COSTAR_NEVER : us * 1000;
}

/* ── Interrupt controller (hw_irq_ctrl_*) ─────────────────────────────── */

static uint64_t irq_status;   /* pending and enabled */
static uint64_t irq_premask;  /* pending, before the enable mask */
static uint64_t irq_mask;     /* enabled */
static bool irqs_locked;
static uint8_t irq_prio[N_IRQS];
static int currently_running_prio = 256; /* 255 is the lowest interrupt priority */

static void hw_irq_ctrl_init(void)
{
    for (int i = 0; i < N_IRQS; i++) {
        irq_prio[i] = 255;
    }
}

static void irq_raise(unsigned int irq)
{
    irq_premask |= (uint64_t)1 << irq;
    if (irq_mask & ((uint64_t)1 << irq)) {
        irq_status |= (uint64_t)1 << irq;
    }
}

void hw_irq_ctrl_set_cur_prio(int new_prio) { currently_running_prio = new_prio; }
int hw_irq_ctrl_get_cur_prio(void) { return currently_running_prio; }
void hw_irq_ctrl_prio_set(unsigned int irq, unsigned int prio) { irq_prio[irq] = (uint8_t)prio; }
uint8_t hw_irq_ctrl_get_prio(unsigned int irq) { return irq_prio[irq]; }
uint32_t hw_irq_ctrl_get_current_lock(void) { return irqs_locked; }
void hw_irq_ctrl_disable_irq(unsigned int irq) { irq_mask &= ~((uint64_t)1 << irq); }
int hw_irq_ctrl_is_irq_enabled(unsigned int irq) { return (irq_mask >> irq) & 1; }
int hw_irq_ctrl_is_irq_pending(unsigned int irq) { return (irq_premask >> irq) & 1; }

int hw_irq_ctrl_get_highest_prio_irq(void)
{
    if (irqs_locked) {
        return -1;
    }

    int winner = -1;
    int winner_prio = 256;
    for (uint64_t pending = irq_status; pending != 0; pending &= pending - 1) {
        int irq = __builtin_ctzll(pending);
        if (winner_prio > irq_prio[irq] && currently_running_prio > irq_prio[irq]) {
            winner = irq;
            winner_prio = irq_prio[irq];
        }
    }
    return winner;
}

uint32_t hw_irq_ctrl_change_lock(uint32_t new_lock)
{
    uint32_t previous_lock = irqs_locked;

    irqs_locked = new_lock;
    if (previous_lock && !new_lock && irq_status != 0) {
        nsif_cpu0_irq_raised_from_sw();
    }
    return previous_lock;
}

void hw_irq_ctrl_clear_irq(unsigned int irq)
{
    irq_status &= ~((uint64_t)1 << irq);
    irq_premask &= ~((uint64_t)1 << irq);
}

void hw_irq_ctrl_raise_im_from_sw(unsigned int irq)
{
    irq_raise(irq);
    if (!irqs_locked) {
        nsif_cpu0_irq_raised_from_sw();
    }
}

void hw_irq_ctrl_enable_irq(unsigned int irq)
{
    irq_mask |= (uint64_t)1 << irq;
    if (irq_premask & ((uint64_t)1 << irq)) {
        hw_irq_ctrl_raise_im_from_sw(irq);
    }
}

/* costar delivers each raised line here when the halted CPU wakes up.  The
 * board's posix_irq_handler() then runs Zephyr's ISRs and any context switch. */
static uint64_t wake_at = NSI_NEVER;

static int on_costar_irq(unsigned int line)
{
    sync_time();
    if (line == WAKE_LINE) {
        wake_at = NSI_NEVER; /* It only wakes the CPU (a "phony" IRQ in native_sim). */
    } else if (line < N_IRQS) {
        irq_raise(line);
    }
    return 0;
}

static void no_switch_context(void)
{
    /* Zephyr switches threads itself, inside posix_irq_handler(). */
}

/* ── Timer (hwtimer_*) ───────────────────────────────────────────────── */

void hwtimer_set_real_time_mode(bool real_time)
{
    (void)real_time; /* costar time is always virtual. */
}

void hwtimer_set_tick_one_shot(uint64_t deadline)
{
    if (deadline < nsi_simu_time) {
        /* A deadline far in the past is a wrapped one: never fire it. */
        deadline = (nsi_simu_time - deadline) > UINT64_MAX / 2 ? NSI_NEVER : nsi_simu_time;
    }
    costar_timer_set(TIMER_TICK_IRQ, to_costar_time(deadline));
}

/* Wake the CPU (without an interrupt) no later than `time`.  Used by
 * busy waits; an earlier pending request prevails. */
void hwtimer_wake_in_time(uint64_t time)
{
    if (time < nsi_simu_time) {
        time = nsi_simu_time;
    }
    if (time < wake_at) {
        wake_at = time;
        costar_timer_set(WAKE_LINE, to_costar_time(time));
    }
}

/* ── CPU (nce_*) ─────────────────────────────────────────────────────── */

static int cpu_state;

void *nce_init(void) { return &cpu_state; }
void nce_terminate(void *this) { (void)this; }
int nce_is_cpu_running(void *this) { (void)this; return 1; }

/* The thread running the reset vector is the CPU: start the kernel on it. */
void nce_boot_cpu(void *this, void (*start_routine)(void))
{
    (void)this;
    start_routine();
}

void nce_halt_cpu(void *this)
{
    (void)this;
    costar_cpu_halt();
    sync_time();
}

void nce_wake_cpu(void *this)
{
    (void)this; /* costar wakes the CPU when one of its IRQ lines is raised. */
}

/* ── Threads (nct_*) ─────────────────────────────────────────────────── */

enum nct_state { NCT_FREE, NCT_USED, NCT_ABORTING };

struct nct_thread {
    costar_thread thread;
    void *payload;
    enum nct_state state;
    int unique_id;
};

static struct nct_thread *nct_threads;
static int nct_capacity;
static int nct_current = -1;
static int nct_next_unique_id;
static void (*nct_thread_fn)(void *);

static void nct_entry(void *arg)
{
    int idx = (int)(intptr_t)arg;

    nct_thread_fn(nct_threads[idx].payload);
    /* Zephyr threads never return from their entry function. */
    costar_cpu_stop(-1);
}

/* Free threads that aborted themselves and have since switched away. */
static void nct_reap(void)
{
    for (int i = 0; i < nct_capacity; i++) {
        if (nct_threads[i].state == NCT_ABORTING && i != nct_current) {
            costar_thread_free(nct_threads[i].thread);
            nct_threads[i].state = NCT_FREE;
        }
    }
}

static int nct_free_slot(void)
{
    for (int i = 0; i < nct_capacity; i++) {
        if (nct_threads[i].state == NCT_FREE) {
            return i;
        }
    }
    int idx = nct_capacity;
    nct_capacity = nct_capacity ? nct_capacity * 2 : 16;
    nct_threads = realloc(nct_threads, (size_t)nct_capacity * sizeof(*nct_threads));
    if (nct_threads == NULL) {
        costar_cpu_stop(-1);
    }
    for (int i = idx; i < nct_capacity; i++) {
        nct_threads[i].state = NCT_FREE;
    }
    return idx;
}

/* Runs when costar unloads this copy of the image. */
__attribute__((destructor)) static void nct_free_table(void)
{
    free(nct_threads);
}

void *nct_init(void (*fptr)(void *))
{
    nct_thread_fn = fptr;
    return &nct_threads;
}

void nct_clean_up(void *this)
{
    (void)this;
}

int nct_new_thread(void *this, void *payload)
{
    (void)this;
    nct_reap();
    int idx = nct_free_slot();
    nct_threads[idx] = (struct nct_thread){
        .thread = costar_thread_new(nct_entry, (void *)(intptr_t)idx, 0),
        .payload = payload,
        .state = NCT_USED,
        .unique_id = nct_next_unique_id++,
    };
    return idx;
}

/* Zephyr chose `next`; run it.  An aborted caller is never resumed. */
void nct_swap_threads(void *this, int next)
{
    (void)this;
    nct_reap();
    nct_current = next;
    costar_thread_switch(nct_threads[next].thread);
}

/* Called once, from the CPU's boot thread, which is never resumed. */
void nct_first_thread_start(void *this, int next)
{
    nct_swap_threads(this, next);
}

void nct_abort_thread(void *this, int idx)
{
    (void)this;
    if (idx == nct_current) {
        nct_threads[idx].state = NCT_ABORTING; /* freed once it has switched away */
    } else if (nct_threads[idx].state == NCT_USED) {
        costar_thread_free(nct_threads[idx].thread);
        nct_threads[idx].state = NCT_FREE;
    }
}

void nct_get_thread_stack(void *this, int idx, void **stack_address, unsigned long *stack_size)
{
    size_t size;

    (void)this;
    costar_thread_stack(nct_threads[idx].thread, stack_address, &size);
    *stack_size = size;
}

int nct_get_unique_thread_id(void *this, int idx)
{
    (void)this;
    return nct_threads[idx].unique_id;
}

int nct_thread_name_set(void *this, int idx, const char *name)
{
    (void)this;
    (void)idx;
    (void)name;
    return 0;
}

/* ── Printing, command line, exit (nsi_*) ────────────────────────────── */

static void log_vprintf(const char *prefix, const char *format, va_list args)
{
    char text[1024];
    int n = snprintf(text, sizeof(text), "%s", prefix);
    n += vsnprintf(text + n, sizeof(text) - (size_t)n, format, args);
    if (n > (int)sizeof(text) - 1) {
        n = (int)sizeof(text) - 1;
    }
    costar_log(text, (size_t)n);
}

/* Like the upstream runner's nsi_exit(): run the embedded image's exit
 * cleanup (ON_EXIT native tasks, ...) once, then stop the CPU. */
void nsi_exit(int exit_code)
{
    static bool exiting;

    if (!exiting) {
        exiting = true;
        int cleanup_code = nsif_cpu0_cleanup();
        if (cleanup_code > exit_code) {
            exit_code = cleanup_code;
        }
    }
    costar_cpu_stop(exit_code);
}

void nsi_vprint_trace(const char *format, va_list args) { log_vprintf("", format, args); }
void nsi_vprint_warning(const char *format, va_list args) { log_vprintf("warning: ", format, args); }

void nsi_vprint_error_and_exit(const char *format, va_list args)
{
    log_vprintf("error: ", format, args);
    nsi_exit(1);
}

int nsi_trace_over_tty(int file_number)
{
    (void)file_number;
    return 0;
}

struct args_struct_t;
void nsi_add_command_line_opts(struct args_struct_t *args)
{
    (void)args;
}

static char *cmd_line[] = { "costar", NULL };

void nsi_get_cmd_line_args(int *argc, char ***argv)
{
    *argc = 1;
    *argv = cmd_line;
}

void nsi_get_test_cmd_line_args(int *argc, char ***argv)
{
    *argc = 0;
    *argv = &cmd_line[1];
}

/* ── Reset vector ────────────────────────────────────────────────────── */

/* Same order as the upstream runner's nsi_init(), for one CPU. */
void costar_image_reset(void)
{
    sync_time();
    costar_cpu_init(on_costar_irq, no_switch_context);
    nsif_cpu0_pre_cmdline_hooks();
    nsif_cpu0_pre_hw_init_hooks();
    hw_irq_ctrl_init();
    nsif_cpu0_boot();  /* Boots Zephyr; does not return. */
}
