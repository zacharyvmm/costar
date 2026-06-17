/* sim_kernel_bridge.c — Simulator kernel bridge */

#include "FreeRTOS.h"
#include "task.h"

#define MAX_BRIDGE_TASKS 16

/* ── Task-to-fiber mapping ────────────────────────────────────────── */

static struct tskTaskControlBlock *bridge_tcbs[MAX_BRIDGE_TASKS];

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

#define MAX_PENDING_TCBS 8

typedef struct PendingTCB {
    struct tskTaskControlBlock *tcb;
} PendingTCB;

PendingTCB pending_tcbs[MAX_PENDING_TCBS];
int pending_count = 0;

void sim_bridge_add_pending_tcb( void *pvTCB )
{
    if( pending_count < MAX_PENDING_TCBS )
    {
        pending_tcbs[pending_count].tcb =
            (struct tskTaskControlBlock *)pvTCB;
        pending_count++;
    }
}
