/*
 * costar_host.h — how a loaded firmware image reaches its machine.
 *
 * Every image is linked with ports/common/image_stub.c, which implements the
 * costar.h functions by forwarding them through this table.  Right after
 * loading an image, costar calls costar_image_attach() with the table and the
 * machine that owns this copy of the image.  Each loaded copy has its own
 * globals, so each copy forwards to its own machine.
 */
#ifndef COSTAR_HOST_H
#define COSTAR_HOST_H

#include "costar.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Bump whenever the table below changes, so stale images are rejected. */
#define COSTAR_HOST_VERSION 2

struct costar_host {
    uint32_t version;
    costar_time (*now)(void *machine);
    void (*cpu_init)(void *machine, costar_isr_fn isr, void (*switch_context)(void));
    void (*cpu_halt)(void *machine);
    void (*cpu_poll)(void *machine, costar_time cost);
    void (*cpu_stop)(void *machine, int status);
    unsigned (*irq_lock)(void *machine);
    void (*irq_unlock)(void *machine, unsigned key);
    void (*timer_set)(void *machine, unsigned irq, costar_time at);
    costar_thread (*thread_new)(void *machine, void (*entry)(void *), void *arg, size_t stack_hint);
    void (*thread_switch)(void *machine, costar_thread next);
    void (*thread_free)(void *machine, costar_thread thread);
    void (*thread_stack)(void *machine, costar_thread thread, void **base, size_t *size);
    void (*trace)(void *machine, const char *label, uint32_t value);
    void (*log)(void *machine, const char *text, size_t length);
};

/* Exported by every image.  Returns 0 on success. */
typedef int (*costar_image_attach_fn)(const struct costar_host *host, void *machine);
#define COSTAR_IMAGE_ATTACH_SYMBOL "costar_image_attach"
/* The image's reset vector, provided by the RTOS port.  It runs on the
 * CPU's first thread and normally never returns. */
typedef void (*costar_image_reset_fn)(void);
#define COSTAR_IMAGE_RESET_SYMBOL "costar_image_reset"

#ifdef __cplusplus
}
#endif

#endif /* COSTAR_HOST_H */
