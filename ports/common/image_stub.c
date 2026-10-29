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
/* Instrumented constructors can poll while the image is being loaded. */
void costar_cpu_poll(costar_time cost) { if (host != NULL) host->cpu_poll(machine, cost); }
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

int costar_dev(const char *name) { return host->dev(machine, name); }
int costar_dev_irq(int dev) { return host->dev_irq(machine, dev); }
int costar_chan_send(int dev, const void *frame, size_t length) { return host->chan_send(machine, dev, frame, length); }
int costar_chan_recv(int dev, void *frame, size_t capacity) { return host->chan_recv(machine, dev, frame, capacity); }
uint32_t costar_reg_read(int dev, uint32_t reg) { return host->reg_read(machine, dev, reg); }
void costar_reg_write(int dev, uint32_t reg, uint32_t value) { host->reg_write(machine, dev, reg, value); }
size_t costar_mem_size(int dev) { return host->mem_size(machine, dev); }
int costar_mem_read(int dev, uint32_t offset, void *buffer, size_t length) { return host->mem_read(machine, dev, offset, buffer, length); }
int costar_mem_write(int dev, uint32_t offset, const void *data, size_t length) { return host->mem_write(machine, dev, offset, data, length); }
int costar_mem_erase(int dev, uint32_t offset, size_t length) { return host->mem_erase(machine, dev, offset, length); }

void costar_trace(const char *label, uint32_t value) { host->trace(machine, label, value); }
void costar_log(const char *text, size_t length) { host->log(machine, text, length); }
