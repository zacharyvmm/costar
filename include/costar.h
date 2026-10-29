/*
 * costar.h — the virtual CPU that firmware runs on.
 *
 * This is the whole interface between a firmware image and costar.  RTOS
 * ports (ports/freertos, ports/zephyr) are written against it; applications
 * normally only use costar_trace().
 *
 * Execution model ("infinitely fast CPU"):
 *   - Virtual time only advances while the CPU is halted or polling.
 *   - The RTOS decides which thread runs; the port switches with
 *     costar_thread_switch().
 *   - Interrupts are delivered at interrupt points: when costar_cpu_halt()
 *     or costar_cpu_poll() returns, and when costar_irq_unlock() re-enables
 *     interrupts.  Each pending IRQ runs the port's ISR entry on the current
 *     thread; afterwards, if any ISR asked for it, costar calls the port's
 *     context-switch function (like PendSV on Cortex-M).
 */
#ifndef COSTAR_H
#define COSTAR_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Virtual time in nanoseconds. */
typedef uint64_t costar_time;
#define COSTAR_NEVER UINT64_MAX

/* Number of interrupt lines per CPU. */
#define COSTAR_IRQ_LINES 64

/* An RTOS thread, backed by a host fiber. */
typedef struct costar_thread_s *costar_thread;

/* Returns non-zero if the ISR made a higher-priority thread ready. */
typedef int (*costar_isr_fn)(unsigned irq);

/* ── CPU ──────────────────────────────────────────────────────────── */

costar_time costar_now(void);

/* Registers the port's interrupt entry and context-switch function. */
void costar_cpu_init(costar_isr_fn isr, void (*switch_context)(void));

/* Wait for interrupt.  Returns immediately if an IRQ is already pending
 * (even if masked), otherwise lets virtual time advance until one is. */
void costar_cpu_halt(void);

/* Charge `cost` of busy time and let the world run (a preemption point for
 * instrumented busy loops). */
void costar_cpu_poll(costar_time cost);

/* Disable interrupts; returns the previous state for costar_irq_unlock(). */
unsigned costar_irq_lock(void);
/* Restore the interrupt state returned by costar_irq_lock(). */
void costar_irq_unlock(unsigned key);
/* Pass to costar_irq_unlock() to enable interrupts unconditionally. */
#define COSTAR_IRQS_ENABLED 1u

/* Power the CPU off for good (firmware exit or fatal error).  Never returns. */
void costar_cpu_stop(int status);

/* One-shot timer: raise `irq` at absolute time `at` (COSTAR_NEVER cancels). */
void costar_timer_set(unsigned irq, costar_time at);

/* ── Threads ──────────────────────────────────────────────────────── */

/* Create a suspended thread that will run entry(arg) when first switched to.
 * `stack_hint` is the RTOS-requested size in bytes; the host may use more. */
costar_thread costar_thread_new(void (*entry)(void *), void *arg, size_t stack_hint);
/* Suspend the calling thread and run `next` (a no-op if `next` is running). */
void costar_thread_switch(costar_thread next);
/* Destroy a thread that is not running. */
void costar_thread_free(costar_thread thread);
/* The host stack a thread runs on (lowest address and size). */
void costar_thread_stack(costar_thread thread, void **base, size_t *size);

/* ── Devices ──────────────────────────────────────────────────────────
 *
 * The board (set up by the host) names each device and gives it an IRQ line.
 * Devices come in three shapes:
 *   channel    frames in and out (UART, CAN, Ethernet, SPI, I2C, ...).  An
 *              arriving frame raises the device's IRQ.
 *   registers  32-bit values by index (GPIO, ADC, sensors, ...).  A value
 *              changed by the host raises the device's IRQ.
 *   memory     bytes (flash, EEPROM, block devices).  Erased bytes read 0xFF.
 * Functions return -1 for an unknown handle or a device of another shape.
 */

/* Look up a device by name.  Returns a handle, or -1 if the board has none. */
int costar_dev(const char *name);
/* The device's IRQ line, or -1 if it has none. */
int costar_dev_irq(int dev);

/* Queue a frame for transmission. */
int costar_chan_send(int dev, const void *frame, size_t length);
/* Take the oldest received frame.  Copies at most `capacity` bytes and
 * returns the frame's length, or -1 if no frame is waiting. */
int costar_chan_recv(int dev, void *frame, size_t capacity);

uint32_t costar_reg_read(int dev, uint32_t reg);
void costar_reg_write(int dev, uint32_t reg, uint32_t value);

size_t costar_mem_size(int dev);
int costar_mem_read(int dev, uint32_t offset, void *buffer, size_t length);
int costar_mem_write(int dev, uint32_t offset, const void *data, size_t length);
int costar_mem_erase(int dev, uint32_t offset, size_t length);

/* ── Observability ────────────────────────────────────────────────── */

void costar_trace(const char *label, uint32_t value);
/* Record a line of text output, such as the board console.  A trailing
 * newline is dropped. */
void costar_log(const char *text, size_t length);

#ifdef __cplusplus
}
#endif

#endif /* COSTAR_H */
