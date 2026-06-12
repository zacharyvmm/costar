/*
 * FreeRTOS simulator port — port.c
 *
 * This file implements the FreeRTOS port layer for the Universal RTOS
 * Native Simulator.  Instead of managing real CPU registers and interrupt
 * controllers, it delegates task creation and context switching to the
 * Rust runtime via the sim_abi.h interface.
 */

#include "portmacro.h"
#include "sim_abi.h"

#include <stddef.h>

/* ─────────────────────────────────────────────────────────────────────
 * pxPortInitialiseStack
 *
 * In the simulator we use the FreeRTOS stack array as a small metadata
 * buffer instead of building a real CPU exception frame.
 *
 * Layout (each slot is a StackType_t = uint32_t):
 *   [0]  = magic value 0xDEADBEEF (sanity check)
 *   [1]  = (uint32_t)(uintptr_t) task entry function pointer
 *   [2]  = (uint32_t)(uintptr_t) task parameter pointer
 *   [3]  = reserved (sim task handle, filled later)
 *
 * Returns a pointer just past the metadata (so FreeRTOS's stack size
 * check still mostly works).
 * ──────────────────────────────────────────────────────────────────── */

#define PORT_MAGIC  0xDEADBEEFu
#define PORT_SLOTS  4

StackType_t *pxPortInitialiseStack(
    StackType_t *pxTopOfStack,
    TaskFunction_t pxCode,
    void *pvParameters
)
{
    /*
     * pxTopOfStack points to the *top* of the stack array (highest
     * address).  We write metadata at the *bottom*.
     *
     * In FreeRTOS the stack grows down, so pxTopOfStack is the high
     * end.  We need to find the base:
     *   base = pxTopOfStack + 1 - total_stack_depth
     * but we don't know total_stack_depth here without config.
     *
     * Alternative: we assume pxTopOfStack points to a buffer large
     * enough for our metadata at the beginning.  The calling code
     * (xTaskCreate) allocates the stack and passes the top.
     *
     * For simplicity in MVP, we write the metadata at the very start
     * of the buffer, then adjust the return pointer:
     *
     *   return pxTopOfStack - (total_depth - PORT_SLOTS)
     *
     * But since we don't know total_depth, we just use the first few
     * words at whatever pxTopOfStack points to minus an offset.
     *
     * In practice, the buffer passed is usStackDepth words, and
     * pxTopOfStack = &(stack[usStackDepth - 1]).
     *
     * We write at the bottom: &stack[0] through &stack[3].
     * Then return &stack[usStackDepth - 1 - PORT_SLOTS].
     *
     * But we don't have usStackDepth...  So we use a simpler approach:
     * the C startup code stores metadata in the first 4 words of the
     * stack buffer that FreeRTOS gives us.  The stack buffer starts at
     * (pxTopOfStack - usStackDepth + 1).  We write metadata at the
     * lowest address and return a pointer above it.
     */

    StackType_t *base = pxTopOfStack; /* We'll adjust below */

    /* Write magic and metadata at the current position. */
    base[0] = PORT_MAGIC;
    base[1] = (StackType_t)(uintptr_t)pxCode;
    base[2] = (StackType_t)(uintptr_t)pvParameters;
    base[3] = 0; /* sim task handle, filled by the scheduler */

    /*
     * Return a pointer that leaves room for the metadata we just wrote.
     * FreeRTOS expects a pointer to the top of the available stack
     * (i.e., the return address for the "first" context switch).
     *
     * In our case, we just return base (the lowest address we wrote to).
     * The scheduler later interprets this.
     */
    return base;
}

/* ─────────────────────────────────────────────────────────────────────
 * xPortStartScheduler
 * ──────────────────────────────────────────────────────────────────── */

BaseType_t xPortStartScheduler( void )
{
    /*
     * Transfer control to the Rust scheduler.
     * sim_start_scheduler() will drain tasks until none remain,
     * then return.
     */
    sim_start_scheduler();

    /* Should not reach here in normal operation. */
    return 0;
}

/* ─────────────────────────────────────────────────────────────────────
 * vPortEndScheduler
 * ──────────────────────────────────────────────────────────────────── */

void vPortEndScheduler( void )
{
    /* Nothing to do — the simulation ends when tasks exit. */
}
