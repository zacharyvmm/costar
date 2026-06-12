#include "FreeRTOS.h"
#include "task.h"
#include "queue.h"
#include "sim_abi.h"

static QueueHandle_t xQueue;

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
        else
            vTaskDelay( 1 );
    }
}

void vApplicationGetIdleTaskMemory( StaticTask_t **a, StackType_t **b, configSTACK_DEPTH_TYPE *c )
{ static StaticTask_t t; static StackType_t s[128]; *a=&t; *b=s; *c=128; }
void vApplicationGetTimerTaskMemory( StaticTask_t **a, StackType_t **b, configSTACK_DEPTH_TYPE *c )
{ static StaticTask_t t; static StackType_t s[128]; *a=&t; *b=s; *c=128; }

int c_sim_main( void )
{
    TaskHandle_t thA, thB;
    sim_task_handle_t hA, hB;

    xQueue = xQueueCreate( 5, sizeof( uint32_t ) );

    /* Create FreeRTOS tasks */
    xTaskCreate( vTaskA, "Sender",   256, NULL, 1, &thA );
    xTaskCreate( vTaskB, "Receiver", 256, NULL, 1, &thB );

    /* Create Rust fibers directly (not via trace hook) */
    hA = sim_create_task( "Sender",   (sim_task_entry_fn)vTaskA, NULL, 256, 1 );
    hB = sim_create_task( "Receiver", (sim_task_entry_fn)vTaskB, NULL, 256, 1 );

    /* Register TCB mappings for sim_set_current_task_by_id */
    sim_bridge_register( hA, thA );
    sim_bridge_register( hB, thB );

    vTaskStartScheduler();
    return 0;
}
