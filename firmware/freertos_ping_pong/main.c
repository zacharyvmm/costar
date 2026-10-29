/*
 * Queue ping-pong — an ordinary FreeRTOS application.
 *
 * The sender pushes 1..5 into a queue, one every SEND_PERIOD_TICKS.  The
 * receiver has the higher priority and blocks on the queue, so each send
 * preempts the sender: "received n" is traced before "sent n".
 */
#include "FreeRTOS.h"
#include "queue.h"
#include "task.h"

#include "costar.h"

#ifndef SEND_PERIOD_TICKS
#define SEND_PERIOD_TICKS 1
#endif

#define MESSAGES 5

static QueueHandle_t xQueue;

static void prvSender( void * pvParameters )
{
    ( void ) pvParameters;

    for( uint32_t ulValue = 1; ulValue <= MESSAGES; ulValue++ )
    {
        xQueueSend( xQueue, &ulValue, portMAX_DELAY );
        costar_trace( "sent", ulValue );
        vTaskDelay( SEND_PERIOD_TICKS );
    }

    costar_trace( "sender_done", MESSAGES );
    vTaskDelete( NULL );
}

static void prvReceiver( void * pvParameters )
{
    ( void ) pvParameters;

    for( int i = 0; i < MESSAGES; i++ )
    {
        uint32_t ulValue;
        xQueueReceive( xQueue, &ulValue, portMAX_DELAY );
        costar_trace( "received", ulValue );
    }

    vTaskDelete( NULL );
}

int main( void )
{
    xQueue = xQueueCreate( MESSAGES, sizeof( uint32_t ) );
    xTaskCreate( prvSender, "sender", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
    xTaskCreate( prvReceiver, "receiver", configMINIMAL_STACK_SIZE, NULL, 3, NULL );
    vTaskStartScheduler();
    return 0;
}
