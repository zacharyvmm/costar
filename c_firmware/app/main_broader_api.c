#include "FreeRTOS.h"
#include "task.h"
#include "queue.h"
#include "semphr.h"
#include "event_groups.h"
#include <stdint.h>
#include "sim_abi.h"

/* ── Shared objects ──────────────────────────────────────────────── */

static SemaphoreHandle_t xBinarySemaphore;
static SemaphoreHandle_t xCountingSemaphore;
static SemaphoreHandle_t xMutex;
static SemaphoreHandle_t xRecursiveMutex;
static EventGroupHandle_t xEventGroup;

#define EVENT_BIT_TASK_A ( 1 << 0 )
#define EVENT_BIT_TASK_B ( 1 << 1 )
#define EVENT_BIT_DONE   ( 1 << 2 )

/* ── Task A: exercises semaphores, mutexes, event groups ──────────── */

static void vTaskA( void *pvParameters )
{
    (void) pvParameters;

    /* 1. Binary semaphore — not given yet, take with zero timeout fails */
    BaseType_t ok = xSemaphoreTake( xBinarySemaphore, 0 );
    (void) ok; /* expect pdFALSE */

    /* 2. Counting semaphore — take from initial count of 3 */
    xSemaphoreTake( xCountingSemaphore, 0 );  /* 3 → 2 */
    xSemaphoreTake( xCountingSemaphore, 0 );  /* 2 → 1 */

    /* 3. Mutex — take and give */
    xSemaphoreTake( xMutex, 0 );
    xSemaphoreGive( xMutex );

    /* 4. Recursive mutex — take twice, give twice */
    xSemaphoreTakeRecursive( xRecursiveMutex, 0 );
    xSemaphoreTakeRecursive( xRecursiveMutex, 0 );
    xSemaphoreGiveRecursive( xRecursiveMutex );
    xSemaphoreGiveRecursive( xRecursiveMutex );

    /* 5. Set event bits to signal Task B */
    xEventGroupSetBits( xEventGroup, EVENT_BIT_TASK_A );
    sim_trace_u32( "taskA_set_bits", EVENT_BIT_TASK_A );

    /* 6. Poll for Task B acknowledgement (non-blocking) */
    {
        EventBits_t bits;
        do {
            bits = xEventGroupWaitBits(
                xEventGroup,
                EVENT_BIT_TASK_B,
                pdTRUE,   /* clear on exit */
                pdTRUE,   /* wait for all bits */
                0         /* non-blocking poll */
            );
            if( ( bits & EVENT_BIT_TASK_B ) != EVENT_BIT_TASK_B )
                vTaskDelay( 1 );
        } while( ( bits & EVENT_BIT_TASK_B ) != EVENT_BIT_TASK_B );
    }

    sim_trace_u32( "taskA_got_ack", 1 );

    /* 7. Signal done */
    xEventGroupSetBits( xEventGroup, EVENT_BIT_DONE );
    sim_trace_u32( "taskA_done", 1 );
}

/* ── Task B: exercises task notifications ────────────────────────── */

extern TaskHandle_t xTaskC;

static void vTaskB( void *pvParameters )
{
    (void) pvParameters;

    /* 1. Poll for Task A's event bit (non-blocking) */
    {
        EventBits_t bits;
        do {
            bits = xEventGroupWaitBits(
                xEventGroup,
                EVENT_BIT_TASK_A,
                pdTRUE,   /* clear on exit */
                pdTRUE,   /* wait for all bits */
                0         /* non-blocking poll */
            );
            if( ( bits & EVENT_BIT_TASK_A ) != EVENT_BIT_TASK_A )
                vTaskDelay( 1 );
        } while( ( bits & EVENT_BIT_TASK_A ) != EVENT_BIT_TASK_A );
    }

    sim_trace_u32( "taskB_got_bits", EVENT_BIT_TASK_A );

    /* 2. Give binary semaphore, then take it */
    xSemaphoreGive( xBinarySemaphore );
    xSemaphoreTake( xBinarySemaphore, 0 );

    /* 3. Counting semaphore give/take */
    xSemaphoreGive( xCountingSemaphore );        /* 1 → 2 */
    xSemaphoreTake( xCountingSemaphore, 0 );     /* 2 → 1 */
    xSemaphoreGive( xCountingSemaphore );        /* 1 → 2 */

    /* 4. Task notification — notify Task C with value 42 */
    xTaskNotify( xTaskC, 42, eSetValueWithOverwrite );
    sim_trace_u32( "taskB_notified_c", 42 );

    /* 5. Acknowledge Task A */
    xEventGroupSetBits( xEventGroup, EVENT_BIT_TASK_B );

    /* 6. Poll for done signal (non-blocking) */
    {
        EventBits_t bits;
        do {
            bits = xEventGroupWaitBits(
                xEventGroup,
                EVENT_BIT_DONE,
                pdFALSE,  /* don't clear on exit */
                pdTRUE,   /* wait for all bits */
                0         /* non-blocking poll */
            );
            if( ( bits & EVENT_BIT_DONE ) != EVENT_BIT_DONE )
                vTaskDelay( 1 );
        } while( ( bits & EVENT_BIT_DONE ) != EVENT_BIT_DONE );
    }

    sim_trace_u32( "taskB_got_done", 1 );
}

/* ── Task C: receives task notification ──────────────────────────── */

TaskHandle_t xTaskC = NULL;

static void vTaskC( void *pvParameters )
{
    (void) pvParameters;
    uint32_t ulNotificationValue = 0;

    /* Poll for notification from Task B (non-blocking) */
    {
        BaseType_t notified;
        do {
            notified = xTaskNotifyWait(
                0,           /* no bits to clear on entry */
                UINT32_MAX,  /* clear all bits on exit */
                &ulNotificationValue,
                0            /* non-blocking poll */
            );
            if( notified != pdTRUE )
                vTaskDelay( 1 );
        } while( notified != pdTRUE );
    }

    sim_trace_u32( "taskC_got_notify", ulNotificationValue );
}

/* ── Main entry point ────────────────────────────────────────────── */

int c_sim_broader_api_main( void )
{
    TaskHandle_t thA, thB, thC;
    sim_task_handle_t hA, hB, hC;

    /* Create kernel objects */
    xBinarySemaphore   = xSemaphoreCreateBinary();
    xCountingSemaphore = xSemaphoreCreateCounting( 3, 3 );
    xMutex             = xSemaphoreCreateMutex();
    xRecursiveMutex    = xSemaphoreCreateRecursiveMutex();
    xEventGroup        = xEventGroupCreate();

    /* Create FreeRTOS tasks */
    xTaskCreate( vTaskA, "TaskA", 512, NULL, 2, &thA );
    xTaskCreate( vTaskB, "TaskB", 512, NULL, 2, &thB );
    xTaskCreate( vTaskC, "TaskC", 512, NULL, 2, &thC );

    xTaskC = thC;

    /* Create Rust fibers (must be done from main, not from trace hook) */
    hA = sim_create_task( "TaskA", (sim_task_entry_fn)vTaskA, NULL, 512, 2 );
    hB = sim_create_task( "TaskB", (sim_task_entry_fn)vTaskB, NULL, 512, 2 );
    hC = sim_create_task( "TaskC", (sim_task_entry_fn)vTaskC, NULL, 512, 2 );

    /* Register TCB mappings */
    sim_bridge_register( hA, thA );
    sim_bridge_register( hB, thB );
    sim_bridge_register( hC, thC );

    vTaskStartScheduler();
    return 0;
}
