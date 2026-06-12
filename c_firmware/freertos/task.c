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
    sim_task_handle_t simHandle;        /* Rust fiber handle (== TaskId) */
} TCB_t;

/* ── Static pools ──────────────────────────────────────────────────── */

#define MAX_TASKS  10

static TCB_t xTCBPool[ MAX_TASKS ];
static StackType_t xStackPool[ MAX_TASKS ][ 256 ];
static int tcbCount = 0;

/* ── Ready lists ───────────────────────────────────────────────────── */

/* One ready list per priority level. */
static List_t pxReadyTasksLists[ configMAX_PRIORITIES ];

static List_t xDelayedTaskList1;
static List_t xDelayedTaskList2;

/* Overflown delayed tasks. */
static List_t * volatile pxDelayedTaskList = NULL;
static List_t * volatile pxOverflowDelayedTaskList = NULL;

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

    if( tcbCount >= MAX_TASKS )
    {
        return pdFALSE;
    }

    pxNewTCB = &xTCBPool[ tcbCount ];
    pxStack  = xStackPool[ tcbCount ];
    tcbCount++;

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
    TCB_t *pxTCB = pxCurrentTCB;
    TickType_t xTimeToWake;

    if( xTicksToDelay == 0 )
    {
        /* Zero delay — just yield. */
        portYIELD();
        return;
    }

    if( pxTCB == NULL )
    {
        /* No current TCB — fall back to plain yield. */
        portYIELD();
        return;
    }

    taskENTER_CRITICAL();
    {
        /* Remove the current task from the ready list. */
        uxListRemove( &( pxTCB->xStateListItem ) );

        /* Compute the wake time and insert into the delayed list. */
        xTimeToWake = xTickCount + xTicksToDelay;

        listSET_LIST_ITEM_VALUE( &( pxTCB->xStateListItem ), xTimeToWake );
        vListInsert( pxDelayedTaskList, &( pxTCB->xStateListItem ) );

        /* Update next unblock time if this is the earliest. */
        if( xNextTaskUnblockTime == 0 || xTimeToWake < xNextTaskUnblockTime )
        {
            xNextTaskUnblockTime = xTimeToWake;
        }
    }
    taskEXIT_CRITICAL();

    /* Tell the Rust scheduler to suspend this fiber until wake time. */
    sim_task_delay_until( (uint64_t) xTimeToWake );

    /* sim_task_delay_until suspends the fiber; execution resumes here
     * after the Rust scheduler wakes us.  At that point the TCB is
     * already back on the ready list (moved by sim_tick_advance). */
}

/* ── Task delay-until ──────────────────────────────────────────────── */

void vTaskDelayUntil( TickType_t *pxPreviousWakeTime, TickType_t xTimeIncrement )
{
    TCB_t *pxTCB = pxCurrentTCB;
    TickType_t xTimeToWake;
    BaseType_t xShouldDelay = pdFALSE;

    if( pxPreviousWakeTime == NULL || xTimeIncrement == 0 )
    {
        /* Invalid parameters — just yield. */
        portYIELD();
        return;
    }

    if( pxTCB == NULL )
    {
        /* No current TCB — fall back to plain yield. */
        portYIELD();
        return;
    }

    taskENTER_CRITICAL();
    {
        /* Advance the previous wake time by the increment. */
        (*pxPreviousWakeTime) += xTimeIncrement;

        /* Handle overflow in the wake-time accumulator. */
        if( (*pxPreviousWakeTime) < xTimeIncrement )
        {
            /* Overflow — use current tick count as the new base. */
            xTimeToWake = xTickCount + xTimeIncrement;
            *pxPreviousWakeTime = xTimeToWake;
        }
        else
        {
            xTimeToWake = *pxPreviousWakeTime;
        }

        /* Only delay if the wake time is still in the future. */
        if( xTimeToWake > xTickCount )
        {
            /* Remove the task from the ready list. */
            if( uxListRemove( &( pxTCB->xStateListItem ) ) == 0 )
            {
                /* Item was not in a list — something is wrong. */
                taskEXIT_CRITICAL();
                return;
            }

            /* Insert into the delayed list. */
            listSET_LIST_ITEM_VALUE( &( pxTCB->xStateListItem ), xTimeToWake );
            vListInsert( pxDelayedTaskList, &( pxTCB->xStateListItem ) );

            /* Update next unblock time. */
            if( xNextTaskUnblockTime == 0 || xTimeToWake < xNextTaskUnblockTime )
            {
                xNextTaskUnblockTime = xTimeToWake;
            }

            xShouldDelay = pdTRUE;
        }
    }
    taskEXIT_CRITICAL();

    if( xShouldDelay == pdTRUE )
    {
        /* Suspend this fiber until the wake time. */
        sim_task_delay_until( (uint64_t) xTimeToWake );
    }
    else
    {
        /* Wake time already passed — just yield. */
        portYIELD();
    }
}

/* ── Yield ─────────────────────────────────────────────────────────── */

void taskYIELD(void)
{
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

/* ── Tick processing ───────────────────────────────────────────────── */

/**
 * Advance the tick count and move any expired tasks from the delayed
 * list back to the ready list.
 *
 * Called by the Rust scheduler from sim_tick_advance().
 * Returns the number of tasks that were woken.
 */
static uint32_t prvProcessTick( void )
{
    TCB_t *pxTCB;
    uint32_t woken = 0;

    xTickCount++;

    /* Check if any tasks have expired in the delayed list. */
    while( listLIST_IS_EMPTY( pxDelayedTaskList ) == pdFALSE )
    {
        /* Peek at the first item's wake time. */
        TickType_t xHeadValue =
            listGET_ITEM_VALUE_OF_HEAD_ENTRY( pxDelayedTaskList );

        if( xHeadValue > xTickCount )
        {
            /* Not yet expired - stop scanning. */
            break;
        }

        /* Get the first item and its owner TCB. */
        pxTCB = ( TCB_t * )
            listGET_OWNER_OF_HEAD_ENTRY( pxDelayedTaskList );

        /* Remove from delayed list. */
        uxListRemove( &( pxTCB->xStateListItem ) );

        /* Add to the ready list. */
        listSET_LIST_ITEM_VALUE( &( pxTCB->xStateListItem ),
                                 pxTCB->uxPriority );
        vListInsertEnd( &( pxReadyTasksLists[ pxTCB->uxPriority ] ),
                        &( pxTCB->xStateListItem ) );

        woken++;
    }

    return woken;
}

/* ── Simulator ABI: called by Rust ─────────────────────────────────── */

/**
 * Set pxCurrentTCB given a Rust task id (== simHandle).
 */
void sim_set_current_task_by_id( uint64_t task_id )
{
    uint32_t i;

    for( i = 0; i < (uint32_t) tcbCount; i++ )
    {
        if( xTCBPool[ i ].simHandle == (sim_task_handle_t) task_id )
        {
            pxCurrentTCB = &xTCBPool[ i ];
            return;
        }
    }

    /* Not found — task may have exited. */
    pxCurrentTCB = NULL;
}

/**
 * Advance the RTOS tick count by one, waking any expired delayed tasks.
 *
 * Called by the Rust scheduler when virtual time crosses a tick
 * boundary.
 *
 * Returns the number of tasks that were woken.
 */
uint32_t sim_tick_advance( void )
{
    return prvProcessTick();
}

/* ── Idle task ─────────────────────────────────────────────────────── */

/*
 * In the simulator port we don't need an idle task because the
 * Rust scheduler loop handles idle detection.
 */
