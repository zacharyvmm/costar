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
#include "sim_port.h"

#include <stddef.h>

/* ─────────────────────────────────────────────────────────────────────
 * Task start frame
 *
 * The task never runs on the FreeRTOS-allocated stack (its fiber has its own
 * host stack), so the only thing written here is a SimPortFrame holding the
 * entry point and parameter.  sim_port_task_created (patched tasks.c) reads
 * it back through pxTopOfStack.
 * ──────────────────────────────────────────────────────────────────── */

StackType_t *pxPortInitialiseStack(
    StackType_t *pxTopOfStack,
    TaskFunction_t pxCode,
    void *pvParameters
)
{
    /* pxTopOfStack is the highest usable, portBYTE_ALIGNMENT-aligned word.
     * Place the frame just below its end, aligned for pointers. */
    uintptr_t end = ( uintptr_t ) ( pxTopOfStack + 1 );
    uintptr_t addr = ( end - sizeof( SimPortFrame ) ) & ~( ( uintptr_t ) portBYTE_ALIGNMENT_MASK );
    SimPortFrame *frame = ( SimPortFrame * ) addr;

    frame->magic = SIM_PORT_FRAME_MAGIC;
    frame->reserved = 0;
    frame->code = pxCode;
    frame->param = pvParameters;

    return ( ( StackType_t * ) frame ) - 1;
}

/* ─────────────────────────────────────────────────────────────────────
 * Yield / critical sections
 *
 * A yield is FreeRTOS asking for a context switch (PendSV on Cortex-M).  The
 * engine performs it: the fiber suspends, the engine runs
 * vTaskSwitchContext() and resumes whichever task FreeRTOS selected.  A yield
 * requested while interrupts are masked is deferred until they are
 * unmasked, as PendSV would be.
 * ──────────────────────────────────────────────────────────────────── */

void vPortYield( void )
{
    sim_port_yield();
}

void vPortEnterCritical( void )
{
    sim_enter_critical();
}

void vPortExitCritical( void )
{
    sim_exit_critical();
}

/* ─────────────────────────────────────────────────────────────────────
 * Scheduler start
 * ──────────────────────────────────────────────────────────────────── */

static int s_start_external = 0;

BaseType_t xPortStartScheduler( void )
{
    /* vTaskStartScheduler() masked interrupts; the first task starts with
     * them enabled, as on hardware. */
    sim_enable_interrupts();

    if( ( s_start_external == 0 ) && ( sim_port_start_scheduler() == 0 ) )
    {
        /* Standalone: the engine runs the scheduler to completion. */
        sim_start_scheduler();
    }

    /* Driven step by step by a Simulator/World: return to the caller. */
    return pdTRUE;
}

void sim_freertos_start_external( void )
{
    s_start_external = 1;
    vTaskStartScheduler();
    s_start_external = 0;
}

void vPortEndScheduler( void )
{
    /* vTaskEndScheduler(): the engine stops scheduling; the calling task
     * never resumes. */
    sim_port_end_scheduler();
}

/* Called by the engine after a task's function returns.  FreeRTOS tasks must
 * not return; real ports trap here (prvTaskExitError).  The simulator deletes
 * the task instead so the rest of the system keeps running. */
void sim_port_task_returned( void )
{
    sim_trace_u32( "task_returned", 1 );
    vTaskDelete( NULL );
}

/* Called by the engine when the current task faulted (e.g. a Rust panic in
 * a callback): FreeRTOS must stop selecting it. */
void sim_freertos_retire_current( void )
{
    vTaskSuspend( NULL );
}

/* configCONTROL_INFINITE_LOOP(): evaluated at the top of every iteration of
 * the idle and timer task loops.  From the idle task, hand control back to
 * the engine so it can advance virtual time. */
int sim_port_loop_iteration( void )
{
    if( xTaskGetCurrentTaskHandle() == xTaskGetIdleTaskHandle() )
    {
        sim_port_idle();
    }

    return 1;
}

/* ─────────────────────────────────────────────────────────────────────
 * Idle / timer task memory (configSUPPORT_STATIC_ALLOCATION)
 *
 * Every simulated machine starts its own scheduler, so each needs its own
 * idle and timer task buffers: a single static buffer would be shared by all
 * machines' idle tasks.  pvPortMalloc() memory belongs to the active
 * machine's kernel context and is released with it.
 * ──────────────────────────────────────────────────────────────────── */

void vApplicationGetIdleTaskMemory( StaticTask_t **ppxIdleTaskTCBBuffer,
                                    StackType_t **ppxIdleTaskStackBuffer,
                                    configSTACK_DEPTH_TYPE *puxIdleTaskStackSize )
{
    *ppxIdleTaskTCBBuffer = ( StaticTask_t * ) pvPortMalloc( sizeof( StaticTask_t ) );
    *ppxIdleTaskStackBuffer = ( StackType_t * ) pvPortMalloc( configMINIMAL_STACK_SIZE * sizeof( StackType_t ) );
    *puxIdleTaskStackSize = configMINIMAL_STACK_SIZE;
}

void vApplicationGetTimerTaskMemory( StaticTask_t **ppxTimerTaskTCBBuffer,
                                     StackType_t **ppxTimerTaskStackBuffer,
                                     configSTACK_DEPTH_TYPE *puxTimerTaskStackSize )
{
    *ppxTimerTaskTCBBuffer = ( StaticTask_t * ) pvPortMalloc( sizeof( StaticTask_t ) );
    *ppxTimerTaskStackBuffer = ( StackType_t * ) pvPortMalloc( configTIMER_TASK_STACK_DEPTH * sizeof( StackType_t ) );
    *puxTimerTaskStackSize = configTIMER_TASK_STACK_DEPTH;
}

uint32_t sim_freertos_tick_rate_hz( void )
{
    return ( uint32_t ) configTICK_RATE_HZ;
}

void sim_freertos_assert_failed( const char *file, int line )
{
    sim_assert_failed( file, ( uint32_t ) line );
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
