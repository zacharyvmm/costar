/* sim_kernel_bridge.c — Simulator kernel bridge */

#include "FreeRTOS.h"
#include "task.h"

#define MAX_BRIDGE_TASKS 16
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
