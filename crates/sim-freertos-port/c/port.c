/*
 * FreeRTOS simulator port — port.c
 *
 * This file implements the FreeRTOS port layer for the Universal RTOS
 * Native Simulator.  Instead of managing real CPU registers and interrupt
 * controllers, it delegates task creation and context switching to the
 * Rust runtime via the sim_abi.h interface.
 */

#include "FreeRTOS.h"
#include "task.h"
#include "portmacro.h"
#include "sim_abi.h"

#include <stddef.h>

/* ─────────────────────────────────────────────────────────────────────
 * Stack frame layout
 *
 * pxPortInitialiseStack writes a small metadata frame at the base of
 * the FreeRTOS-allocated stack.  sim_port_task_created (in tasks.c)
 * reads it back to create the corresponding Rust fiber.
 *
 * Layout (each slot is StackType_t = uint32_t):
 *   [-3] = reserved (sim task handle, filled by sim_port_task_created)
 *   [-2] = task parameter pointer
 *   [-1] = task entry function pointer
 *   [ 0] = magic value 0xDEADBEEF (sanity check)
 * ──────────────────────────────────────────────────────────────────── */

#define PORT_MAGIC       0xDEADBEEFu
#define PORT_STACK_SLOTS 4

StackType_t *pxPortInitialiseStack(
    StackType_t *pxTopOfStack,
    TaskFunction_t pxCode,
    void *pvParameters
)
{
    StackType_t *sp = pxTopOfStack;

    /* Build a minimal initial stack frame.
     * Real FreeRTOS ports build a CPU exception frame here.
     * For the simulator, we just leave room for our metadata
     * and return a pointer that FreeRTOS will use as the
     * initial stack pointer. */

    sp[-0] = PORT_MAGIC;                            /* magic sentinel */
    sp[-1] = (StackType_t)(uintptr_t)pxCode;        /* task entry */
    sp[-2] = (StackType_t)(uintptr_t)pvParameters;  /* task param */
    sp[-3] = 0;                                     /* simHandle (filled later) */

    /* Return pointer past our metadata so FreeRTOS's stack
     * overflow checks see the expected free space. */
    if( pxCode == 0 || ((uintptr_t)pxCode & 1) ) { /* idle task hack: exit */ }
    return &sp[-PORT_STACK_SLOTS];
}

/* ─────────────────────────────────────────────────────────────────────
 * vPortYield
 *
 * Suspends the active Rust fiber.  Called from portYIELD() / taskYIELD()
 * and portYIELD_WITHIN_API().
 * ──────────────────────────────────────────────────────────────────── */

void vPortYield( void )
{
    sim_port_yield();
}

/* ─────────────────────────────────────────────────────────────────────
 * vPortEnterCritical / vPortExitCritical
 * ──────────────────────────────────────────────────────────────────── */

void vPortEnterCritical( void )
{
    sim_enter_critical();
}

void vPortExitCritical( void )
{
    sim_exit_critical();
}

/* ─────────────────────────────────────────────────────────────────────
 * xPortStartScheduler
 *
 * Transfer control to the Rust scheduler.
 * ──────────────────────────────────────────────────────────────────── */

BaseType_t xPortStartScheduler( void )
{
    sim_start_scheduler();

    /* Should not reach here. */
    return 0;
}

/* ─────────────────────────────────────────────────────────────────────
 * vPortEndScheduler
 * ──────────────────────────────────────────────────────────────────── */

void vPortEndScheduler( void )
{
    /* Nothing to do — the simulation ends when all tasks exit. */
}

/* ─────────────────────────────────────────────────────────────────────
 * sim_tick_advance
 *
 * Called by the Rust scheduler at each virtual tick boundary.
 * Uses real FreeRTOS's xTaskIncrementTick() to advance xTickCount
 * and wake any expired delayed tasks.
 * ──────────────────────────────────────────────────────────────────── */

uint32_t sim_tick_advance( void )
{
    /* xTaskIncrementTick() is a public FreeRTOS function. */
    BaseType_t switch_needed = xTaskIncrementTick();

    (void)switch_needed;
    return 0;
}

/* ─────────────────────────────────────────────────────────────────────
 * sim_advance_ticks
 *
 * Batch-advance the tick count by `count` ticks.  Provides the same
 * logical result as calling sim_tick_advance() `count` times, but
 * with a single C↔Rust crossing.  Used by the tickless-idle fast-forward.
 *
 * Returns the number of context-switch requests signalled across all
 * the batched calls.  A return value > 0 indicates that at least one
 * delayed task was woken and the Rust scheduler should re-scan for
 * runnable tasks.
 * ──────────────────────────────────────────────────────────────────── */

uint32_t sim_advance_ticks( uint32_t count )
{
    uint32_t switches_needed = 0;

    for( uint32_t i = 0; i < count; i++ )
    {
        if( xTaskIncrementTick() != pdFALSE )
        {
            switches_needed++;
        }
    }

    return switches_needed;
}

/* ─────────────────────────────────────────────────────────────────────
 * Memory allocation (for FreeRTOS dynamic allocation)
 * ──────────────────────────────────────────────────────────────────── */

#include <stdlib.h>

void *pvPortMalloc( size_t xWantedSize )
{
    /*
     * Attribute every dynamic FreeRTOS object (TCBs, stacks, queues, timers)
     * to the active Simulator.  Context destruction releases objects that
     * firmware did not delete itself, preventing stale C allocations from
     * surviving a dropped World.
     */
    return sim_freertos_alloc( xWantedSize );
}

void vPortFree( void *pv )
{
    sim_freertos_free( pv );
}
