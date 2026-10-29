/*
 * Link node — an interrupt-driven FreeRTOS application.
 *
 * Board: a "link" channel with an IRQ, and a "node" register device whose
 * register 0 selects the role (0 = initiator, otherwise responder).
 *
 * The initiator sends "P<n>" every 10 ms.  Every node answers a "P<n>" with
 * "R<n>".  Frames are handled the way real drivers do it: the link's ISR wakes
 * a task with vTaskNotifyGiveFromISR(), and the task drains the channel.
 */
#include "FreeRTOS.h"
#include "task.h"

#include "costar.h"

#define PINGS 3

static int xLink;
static TaskHandle_t xHandlerTask;

static void prvLinkInterrupt( void )
{
    BaseType_t xWoken = pdFALSE;

    vTaskNotifyGiveFromISR( xHandlerTask, &xWoken );
    portYIELD_FROM_ISR( xWoken );
}

static void prvHandler( void * pvParameters )
{
    ( void ) pvParameters;

    for( ;; )
    {
        uint8_t ucFrame[ 2 ];

        ulTaskNotifyTake( pdTRUE, portMAX_DELAY );

        while( costar_chan_recv( xLink, ucFrame, sizeof( ucFrame ) ) == 2 )
        {
            if( ucFrame[ 0 ] == 'P' )
            {
                const uint8_t ucReply[ 2 ] = { 'R', ucFrame[ 1 ] };
                costar_trace( "got_ping", ucFrame[ 1 ] );
                costar_chan_send( xLink, ucReply, sizeof( ucReply ) );
            }
            else
            {
                costar_trace( "got_pong", ucFrame[ 1 ] );
            }
        }
    }
}

static void prvInitiator( void * pvParameters )
{
    ( void ) pvParameters;

    for( uint8_t n = 1; n <= PINGS; n++ )
    {
        const uint8_t ucPing[ 2 ] = { 'P', n };
        costar_chan_send( xLink, ucPing, sizeof( ucPing ) );
        vTaskDelay( pdMS_TO_TICKS( 10 ) );
    }

    vTaskDelete( NULL );
}

int main( void )
{
    xLink = costar_dev( "link" );
    vPortSetInterruptHandler( ( unsigned ) costar_dev_irq( xLink ), prvLinkInterrupt );

    xTaskCreate( prvHandler, "handler", configMINIMAL_STACK_SIZE, NULL, 3, &xHandlerTask );

    if( costar_reg_read( costar_dev( "node" ), 0 ) == 0 )
    {
        xTaskCreate( prvInitiator, "initiator", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
    }

    vTaskStartScheduler();
    return 0;
}
