/*
 * task.c — Task management (simulator port)
 *
 * This implementation delegates task lifecycle to the Rust runtime
 * via sim_abi.h.  FreeRTOS concepts (TCB, ready lists, delay lists)
 * are maintained in C for compatibility with existing FreeRTOS code,
 * but the actual context switching is handled by Rust fibers.
 */

#include "FreeRTOS.h"
#include "task.h"
#include "list.h"
#include "sim_abi.h"

#include <stddef.h>
#include <string.h>

/* ── TCB (Task Control Block) ──────────────────────────────────────── */

typedef struct tskTaskControlBlock
{
    StackType_t     *pxTopOfStack;      /* Top of the task's stack */
    ListItem_t      xStateListItem;     /* Links into ready/delay lists */
    ListItem_t      xEventListItem;     /* Links into event lists */
    UBaseType_t     uxPriority;         /* Task priority */
    StackType_t     *pxStack;           /* Base of stack */
    char            pcTaskName[ 16 ];   /* Task name */
    sim_task_handle_t simHandle;        /* Rust fiber handle */
} TCB_t;

/* ── Ready lists ───────────────────────────────────────────────────── */

/* One ready list per priority level. */
static List_t pxReadyTasksLists[ configMAX_PRIORITIES ];

static List_t xDelayedTaskList1;
static List_t xDelayedTaskList2;

/* Overflown delayed tasks. */
static List_t * volatile pxDelayedTaskList;
static List_t * volatile pxOverflowDelayedTaskList;

/* Pointer to the currently executing TCB. */
static TCB_t * volatile pxCurrentTCB = NULL;

/* Number of tasks. */
static volatile UBaseType_t uxCurrentNumberOfTasks = 0;

/* Next unblock time (for tick optimization). */
static volatile TickType_t xNextTaskUnblockTime = 0;

/* Scheduler suspended count. */
static volatile UBaseType_t uxSchedulerSuspended = 0;

/* Tick count. */
static volatile TickType_t xTickCount = 0;

/* ── Startup ────────────────────────────────────────────────────────── */

/** Initialize the scheduler data structures. */
void prvInitialiseTaskLists( void )
{
    UBaseType_t i;

    for( i = 0; i < configMAX_PRIORITIES; i++ )
    {
        vListInitialise( &( pxReadyTasksLists[ i ] ) );
    }

    vListInitialise( &xDelayedTaskList1 );
    vListInitialise( &xDelayedTaskList2 );

    (void) xDelayedTaskList1;
    (void) xDelayedTaskList2;

    pxDelayedTaskList = &xDelayedTaskList1;
    pxOverflowDelayedTaskList = &xDelayedTaskList2;
}

/* ── Task creation ─────────────────────────────────────────────────── */

BaseType_t xTaskCreate(
    TaskFunction_t pxTaskCode,
    const char * const pcName,
    uint16_t usStackDepth,
    void *pvParameters,
    UBaseType_t uxPriority,
    TaskHandle_t *pxCreatedTask
)
{
    TCB_t *pxNewTCB;
    StackType_t *pxStack;

    /* Allocate TCB and stack statically for MVP (no heap). */
    static TCB_t xTCBPool[ 10 ];
    static StackType_t xStackPool[ 10 ][ 256 ];
    static int tcbIndex = 0;
    static int stackIndex = 0;

    if( tcbIndex >= 10 || stackIndex >= 10 )
    {
        return pdFALSE;
    }

    pxNewTCB = &xTCBPool[ tcbIndex++ ];
    pxStack = xStackPool[ stackIndex++ ];

    (void) usStackDepth; /* We use a fixed pool for MVP */

    /* Clear TCB. */
    memset( pxNewTCB, 0, sizeof( TCB_t ) );

    /* Copy name. */
    if( pcName != NULL )
    {
        strncpy( pxNewTCB->pcTaskName, pcName, 15 );
        pxNewTCB->pcTaskName[ 15 ] = '\0';
    }

    pxNewTCB->uxPriority = uxPriority;
    pxNewTCB->pxStack = pxStack;

    /* Initialise the stack (stores metadata for Rust). */
    pxNewTCB->pxTopOfStack = pxPortInitialiseStack(
        &pxStack[ 255 ], /* top of stack = last element */
        pxTaskCode,
        pvParameters
    );

    /* Register with the Rust runtime. */
    sim_task_handle_t simHandle = sim_create_task(
        pxNewTCB->pcTaskName,
        (sim_task_entry_fn) pxTaskCode,
        pvParameters,
        usStackDepth,
        (uint32_t) uxPriority
    );

    if( simHandle == 0 )
    {
        return pdFALSE;
    }

    pxNewTCB->simHandle = simHandle;

    /* Initialise list items. */
    vListInitialiseItem( &( pxNewTCB->xStateListItem ) );
    vListInitialiseItem( &( pxNewTCB->xEventListItem ) );

    listSET_LIST_ITEM_OWNER( &( pxNewTCB->xStateListItem ), pxNewTCB );
    listSET_LIST_ITEM_VALUE( &( pxNewTCB->xStateListItem ), uxPriority );

    /* Add to the ready list. */
    vListInsertEnd( &( pxReadyTasksLists[ uxPriority ] ),
                    &( pxNewTCB->xStateListItem ) );

    uxCurrentNumberOfTasks++;

    if( pxCreatedTask != NULL )
    {
        *pxCreatedTask = ( TaskHandle_t ) pxNewTCB;
    }

    return pdPASS;
}

/* ── Task delay ────────────────────────────────────────────────────── */

void vTaskDelay( TickType_t xTicksToDelay )
{
    /*
     * MVP implementation: just yield cooperatively.
     *
     * Full implementation would:
     * 1. Remove the current task from the ready list
     * 2. Insert into the delayed list sorted by wake time
     * 3. Request a context switch
     *
     * This requires `pxCurrentTCB` to be set, which requires
     * integration between the Rust fiber scheduler and the
     * FreeRTOS TCB.  Deferred to post-MVP.
     */
    (void) xTicksToDelay;
    portYIELD();
}

/* ── Yield ─────────────────────────────────────────────────────────── */

void taskYIELD(void)
{
    /* The Rust fiber will yield and the scheduler will pick
     * the next task. */
    sim_port_yield();
}

/* ── Scheduler control ─────────────────────────────────────────────── */

void vTaskStartScheduler(void)
{
    /* Start the Rust scheduler.  This call never returns
     * until the simulation ends. */
    sim_start_scheduler();
}

void vTaskEndScheduler(void)
{
    /* Nothing to do. */
}

/* ── Suspend / resume scheduler ────────────────────────────────────── */

void vTaskSuspendAll(void)
{
    uxSchedulerSuspended++;
}

BaseType_t xTaskResumeAll(void)
{
    if( uxSchedulerSuspended > 0 )
    {
        uxSchedulerSuspended--;
    }
    return ( uxSchedulerSuspended == 0 ) ? pdTRUE : pdFALSE;
}

/* ── Delete task ───────────────────────────────────────────────────── */

void vTaskDelete( TaskHandle_t xTaskToDelete )
{
    TCB_t *pxTCB = ( TCB_t * ) xTaskToDelete;

    taskENTER_CRITICAL();
    {
        uxListRemove( &( pxTCB->xStateListItem ) );
        uxCurrentNumberOfTasks--;
    }
    taskEXIT_CRITICAL();

    /* If deleting self, yield (task will not be rescheduled). */
    if( pxTCB == pxCurrentTCB )
    {
        sim_task_exit();
    }
}

/* ── Getters ───────────────────────────────────────────────────────── */

TaskHandle_t xTaskGetCurrentTaskHandle(void)
{
    return ( TaskHandle_t ) pxCurrentTCB;
}

TickType_t xTaskGetTickCount(void)
{
    return xTickCount;
}

/* ── Idle task ─────────────────────────────────────────────────────── */

/*
 * In the simulator port we don't need an idle task because the
 * Rust scheduler loop handles idle detection.
 */
