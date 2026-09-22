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


int c_sim_main( void )
{
    TaskHandle_t thA, thB;

    xQueue = xQueueCreate( 5, sizeof( uint32_t ) );

    /* Create FreeRTOS tasks */
    xTaskCreate( vTaskA, "Sender",   256, NULL, 1, &thA );
    xTaskCreate( vTaskB, "Receiver", 256, NULL, 1, &thB );

    vTaskStartScheduler();
    return 0;
}
