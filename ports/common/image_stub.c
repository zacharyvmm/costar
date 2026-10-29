/*
 * image_stub.c — linked into every firmware image.
 *
 * Implements costar.h by forwarding each call to the machine that loaded this
 * copy of the image (see costar_host.h).
 */
#include "costar_host.h"

static const struct costar_host *host;
static void *machine;

__attribute__((visibility("default"))) int costar_image_attach(const struct costar_host *h, void *m)
{
    if (h == NULL || h->version != COSTAR_HOST_VERSION) {
        return -1;
    }
    host = h;
    machine = m;
    return 0;
}

costar_time costar_now(void) { return host->now(machine); }
void costar_cpu_init(costar_isr_fn isr, void (*switch_context)(void)) { host->cpu_init(machine, isr, switch_context); }
void costar_cpu_halt(void) { host->cpu_halt(machine); }
void costar_cpu_poll(costar_time cost) { host->cpu_poll(machine, cost); }
void costar_cpu_stop(int status) { host->cpu_stop(machine, status); }
unsigned costar_irq_lock(void) { return host->irq_lock(machine); }
void costar_irq_unlock(unsigned key) { host->irq_unlock(machine, key); }
void costar_timer_set(unsigned irq, costar_time at) { host->timer_set(machine, irq, at); }

costar_thread costar_thread_new(void (*entry)(void *), void *arg, size_t stack_hint)
{
    return host->thread_new(machine, entry, arg, stack_hint);
}
void costar_thread_switch(costar_thread next) { host->thread_switch(machine, next); }
void costar_thread_free(costar_thread thread) { host->thread_free(machine, thread); }
void costar_thread_stack(costar_thread thread, void **base, size_t *size) { host->thread_stack(machine, thread, base, size); }

void costar_trace(const char *label, uint32_t value) { host->trace(machine, label, value); }
void costar_log(const char *text, size_t length) { host->log(machine, text, length); }
