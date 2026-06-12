/*
 * main.c — Sample FreeRTOS application for the native simulator.
 *
 * Two tasks:
 *   Task A (Sender): sends 5 incrementing counter values to a queue,
 *                    delaying 1 tick between each.
 *   Task B (Receiver): receives 5 values from the queue, yielding
 *                      after each receive.
 *
 * Both tasks exit after their work is done, demonstrating task exit
 * through the Rust fiber runtime.
 */

#include "FreeRTOS.h"
#include "task.h"
#include "queue.h"

/* ── Shared queue ──────────────────────────────────────────────────── */

static QueueHandle_t xQueue;

/* ── Task A: sender (5 iterations then exits) ──────────────────────── */

static void vTaskA( void *pvParameters )
{
    uint32_t ulCounter = 0;
    int i;

    (void) pvParameters;

    for( i = 0; i < 5; i++ )
    {
        ulCounter++;
        xQueueSend( xQueue, &ulCounter, 0 );
        vTaskDelay( 1 );
    }
}

/* ── Task B: receiver (receives 5 values then exits) ───────────────── */

static void vTaskB( void *pvParameters )
{
    uint32_t ulReceived;
    int received = 0;

    (void) pvParameters;

    while( received < 5 )
    {
        if( xQueueReceive( xQueue, &ulReceived, 0 ) == pdPASS )
        {
            received++;
            (void) ulReceived;
        }
        taskYIELD();
    }
}

/* ── Startup hook ──────────────────────────────────────────────────── */

void vApplicationStartupHook(void)
{
    /* No-op in MVP. */
}

/* ── Simulator entry (called from Rust main) ──────────────────────── */

int c_sim_main( void )
{
    /* Initialize scheduler lists. */
    prvInitialiseTaskLists();

    /* Create the queue (holds 5 uint32_t values). */
    xQueue = xQueueCreate( 5, sizeof( uint32_t ) );

    /* Create Task A (sender, priority 1). */
    if( xTaskCreate( vTaskA, "Sender", 256, NULL, 1, NULL ) != pdPASS )
    {
        return 1;
    }

    /* Create Task B (receiver, priority 1). */
    if( xTaskCreate( vTaskB, "Receiver", 256, NULL, 1, NULL ) != pdPASS )
    {
        return 1;
    }

    /* Start the scheduler — this runs until all tasks exit. */
    vTaskStartScheduler();

    return 0;
}
