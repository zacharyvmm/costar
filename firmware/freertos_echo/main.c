/*
 * Echo — a FreeRTOS console on a "uart" channel with an IRQ.
 *
 * The UART's ISR wakes a task, which sends every frame it receives straight
 * back in upper case.
 */
#include <ctype.h>

#include "FreeRTOS.h"
#include "task.h"

#include "costar.h"

static int xUart;
static TaskHandle_t xEchoTask;

static void prvUartInterrupt( void )
{
    BaseType_t xWoken = pdFALSE;

    vTaskNotifyGiveFromISR( xEchoTask, &xWoken );
    portYIELD_FROM_ISR( xWoken );
}

static void prvEcho( void * pvParameters )
{
    static uint8_t ucFrame[ 1024 ];

    ( void ) pvParameters;

    for( ;; )
    {
        int lLength;

        ulTaskNotifyTake( pdTRUE, portMAX_DELAY );

        while( ( lLength = costar_chan_recv( xUart, ucFrame, sizeof( ucFrame ) ) ) > 0 )
        {
            if( lLength > ( int ) sizeof( ucFrame ) )
            {
                lLength = sizeof( ucFrame ); /* the rest of a longer frame is lost */
            }

            for( int i = 0; i < lLength; i++ )
            {
                ucFrame[ i ] = ( uint8_t ) toupper( ucFrame[ i ] );
            }

            costar_chan_send( xUart, ucFrame, ( size_t ) lLength );
        }
    }
}

int main( void )
{
    xUart = costar_dev( "uart" );
    vPortSetInterruptHandler( ( unsigned ) costar_dev_irq( xUart ), prvUartInterrupt );
    xTaskCreate( prvEcho, "echo", configMINIMAL_STACK_SIZE, NULL, 2, &xEchoTask );
    vTaskStartScheduler();
    return 0;
}
