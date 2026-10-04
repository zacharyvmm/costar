#include "FreeRTOS.h"
#include "task.h"
#include <stdint.h>
#include "sim_abi.h"

static void vTaskA( void *pvParameters );
static void vTaskB( void *pvParameters );
static void vTaskC( void *pvParameters );

/* ── Static allocation buffers for Task A ─────────────────────────── */

static StackType_t xTaskAStack[256];
static StaticTask_t xTaskATCB;

/* ── Task B handle ────────────────────────────────────────────────── */

static TaskHandle_t xTaskBHandle;

/* ── Task A: Deleter (priority 2, static allocation) ────────────────
 *
 * Yields to let Tasks B and C run, then deletes Task B.
 */
static void vTaskA( void *pvParameters )
{
    (void) pvParameters;
    sim_trace_u32( "taskA_start", 0 );

    /* Yield so Tasks B and C can run. */
    vTaskDelay( 1 );

    sim_trace_u32( "taskA_delete_B", 0 );
    vTaskDelete( xTaskBHandle );

    sim_trace_u32( "taskA_done", 0 );
}

/* ── Task B: Deleted by Task A (priority 1) ───────────────────────── */
static void vTaskB( void *pvParameters )
{
    (void) pvParameters;
    sim_trace_u32( "taskB_running", 1 );
    vTaskDelay( 1 );
    /* Should not reach here — Task A deletes us before we wake. */
    sim_trace_u32( "taskB_still_alive", 99 );
}

/* ── Task C: Observer (priority 1) ────────────────────────────────── */
static void vTaskC( void *pvParameters )
{
    (void) pvParameters;
    sim_trace_u32( "taskC_observer", 1 );
    sim_trace_u32( "taskC_done", 0 );
}

/* ── Entry point ──────────────────────────────────────────────────── */

int c_sim_task_delete_main( void )
{
    TaskHandle_t thB, thC;

    /* Task A — static allocation */
    ( void ) xTaskCreateStatic( vTaskA, "TaskA", 256, NULL, 2,
                                xTaskAStack, &xTaskATCB );

    /* Task B — will be deleted by Task A */
    xTaskCreate( vTaskB, "TaskB", 256, NULL, 1, &thB );
    xTaskBHandle = thB;

    /* Task C — observer, runs after B */
    xTaskCreate( vTaskC, "TaskC", 256, NULL, 1, &thC );

    vTaskStartScheduler();
    return 0;
}
