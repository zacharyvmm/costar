/* sim_kernel_bridge.c — Simulator kernel bridge */

#include "FreeRTOS.h"
#include "task.h"

#include <stdlib.h>
#include <string.h>

#define MAX_BRIDGE_TASKS 16
#define MAX_PENDING_TCBS 8

/* ── Per-simulator kernel context ─────────────────────────────────── */

/*
 * FreeRTOS's scheduler state lives in file-static variables in tasks.c and
 * timers.c.  Rust can activate a different Simulator between two scheduler
 * calls, so keeping only one C kernel makes task IDs and TCBs cross worlds.
 * The generated hooks in those translation units copy their mutable statics
 * into this opaque context on every activation switch.
 */
extern void *sim_freertos_task_state_create( void );
extern void sim_freertos_task_state_destroy( void *state );
extern void sim_freertos_task_state_save( void *state );
extern void sim_freertos_task_state_restore( const void *state );
extern void *sim_freertos_timer_state_create( void );
extern void sim_freertos_timer_state_destroy( void *state );
extern void sim_freertos_timer_state_save( void *state );
extern void sim_freertos_timer_state_restore( const void *state );

typedef struct SimFreeRtosContext
{
    void *task_state;
    void *timer_state;
    struct tskTaskControlBlock *bridge_tcbs[ MAX_BRIDGE_TASKS ];
    struct tskTaskControlBlock *pending_tcbs[ MAX_PENDING_TCBS ];
    int pending_count;
    struct SimFreeRtosAllocation *allocations;
} SimFreeRtosContext;

static SimFreeRtosContext *active_context = NULL;
struct tskTaskControlBlock *bridge_tcbs[ MAX_BRIDGE_TASKS ];
struct tskTaskControlBlock *pending_tcbs[ MAX_PENDING_TCBS ];
int pending_count = 0;

typedef struct SimFreeRtosAllocation
{
    struct SimFreeRtosAllocation *next;
    struct SimFreeRtosAllocation **previous_next;
    SimFreeRtosContext *owner;
} SimFreeRtosAllocation;

void *sim_freertos_alloc( size_t size )
{
    SimFreeRtosAllocation *allocation =
        ( SimFreeRtosAllocation * ) malloc( sizeof( *allocation ) + size );
    if( allocation == NULL )
    {
        return NULL;
    }

    allocation->owner = active_context;
    allocation->next = NULL;
    allocation->previous_next = NULL;
    if( active_context != NULL )
    {
        allocation->next = active_context->allocations;
        allocation->previous_next = &active_context->allocations;
        if( allocation->next != NULL )
        {
            allocation->next->previous_next = &allocation->next;
        }
        active_context->allocations = allocation;
    }
    return allocation + 1;
}

void sim_freertos_free( void *ptr )
{
    SimFreeRtosAllocation *allocation;
    if( ptr == NULL )
    {
        return;
    }
    allocation = ( ( SimFreeRtosAllocation * ) ptr ) - 1;
    if( allocation->previous_next != NULL )
    {
        *allocation->previous_next = allocation->next;
        if( allocation->next != NULL )
        {
            allocation->next->previous_next = allocation->previous_next;
        }
    }
    free( allocation );
}

void *sim_freertos_context_create( void )
{
    SimFreeRtosContext *context =
        ( SimFreeRtosContext * ) calloc( 1, sizeof( *context ) );

    if( context == NULL )
    {
        return NULL;
    }

    context->task_state = sim_freertos_task_state_create();
    context->timer_state = sim_freertos_timer_state_create();
    if( ( context->task_state == NULL ) || ( context->timer_state == NULL ) )
    {
        sim_freertos_task_state_destroy( context->task_state );
        sim_freertos_timer_state_destroy( context->timer_state );
        free( context );
        return NULL;
    }
    return context;
}

void *sim_freertos_context_activate( void *opaque_context )
{
    SimFreeRtosContext *next = ( SimFreeRtosContext * ) opaque_context;
    SimFreeRtosContext *prior = active_context;

    if( next == prior )
    {
        return prior;
    }

    if( prior != NULL )
    {
        sim_freertos_task_state_save( prior->task_state );
        sim_freertos_timer_state_save( prior->timer_state );
        memcpy( prior->bridge_tcbs, bridge_tcbs, sizeof( bridge_tcbs ) );
        memcpy( prior->pending_tcbs, pending_tcbs, sizeof( pending_tcbs ) );
        prior->pending_count = pending_count;
    }

    active_context = next;
    if( next != NULL )
    {
        sim_freertos_task_state_restore( next->task_state );
        sim_freertos_timer_state_restore( next->timer_state );
        memcpy( bridge_tcbs, next->bridge_tcbs, sizeof( bridge_tcbs ) );
        memcpy( pending_tcbs, next->pending_tcbs, sizeof( pending_tcbs ) );
        pending_count = next->pending_count;
    }
    else
    {
        /* A zero snapshot is the kernel's power-on state. */
        sim_freertos_task_state_restore( NULL );
        sim_freertos_timer_state_restore( NULL );
        memset( bridge_tcbs, 0, sizeof( bridge_tcbs ) );
        memset( pending_tcbs, 0, sizeof( pending_tcbs ) );
        pending_count = 0;
    }
    return prior;
}

void sim_freertos_context_destroy( void *opaque_context )
{
    SimFreeRtosContext *context = ( SimFreeRtosContext * ) opaque_context;
    if( context == NULL )
    {
        return;
    }

    /* Destroying an active context would discard the currently loaded C
     * kernel.  Rust deactivates its guard before dropping Simulator. */
    configASSERT( context != active_context );
    while( context->allocations != NULL )
    {
        SimFreeRtosAllocation *allocation = context->allocations;
        context->allocations = allocation->next;
        free( allocation );
    }
    sim_freertos_task_state_destroy( context->task_state );
    sim_freertos_timer_state_destroy( context->timer_state );
    free( context );
}

/* ── Task-to-fiber mapping ────────────────────────────────────────── */

void sim_bridge_register( uint64_t task_id, void *tcb )
{
    if( task_id > 0 && task_id < MAX_BRIDGE_TASKS )
        bridge_tcbs[task_id] = (struct tskTaskControlBlock *)tcb;
}

/* Called by Rust scheduler before resuming a fiber */
extern struct tskTaskControlBlock * volatile pxCurrentTCB;

void sim_set_current_task_by_id( uint64_t task_id )
{
    if( task_id > 0 && task_id < MAX_BRIDGE_TASKS )
        pxCurrentTCB = bridge_tcbs[task_id];
    else
        pxCurrentTCB = NULL;
}

/* Find the task_id for a given TCB pointer.
 * Used by traceTASK_DELETE to notify the Rust side which fiber
 * to mark as Exited.  Returns 0 if the TCB is not in the table. */
uint64_t sim_bridge_find_task_id( void *tcb )
{
    uint64_t i;
    for( i = 1; i < MAX_BRIDGE_TASKS; i++ )
    {
        if( bridge_tcbs[i] == (struct tskTaskControlBlock *)tcb )
            return i;
    }
    return 0;
}

/* ── Deferred fiber creation (for tasks created by FreeRTOS itself) ──
 *
 * The timer daemon task and idle tasks are created by FreeRTOS inside
 * vTaskStartScheduler(), before xPortStartScheduler() gives control to
 * Rust.  We cannot create corosensei fibers for them at TCB-creation
 * time (deep call stack causes segfault on resume), so we record them
 * in a pending list.
 *
 * sim_bridge_create_pending_fibers() is defined in tasks.c (it needs
 * access to the private TCB struct fields).  This file provides the
 * storage and the sim_bridge_add_pending_tcb() recording function.
 */

void sim_bridge_add_pending_tcb( void *pvTCB )
{
    if( pending_count < MAX_PENDING_TCBS )
    {
        pending_tcbs[pending_count] = (struct tskTaskControlBlock *)pvTCB;
        pending_count++;
    }
}
