/*
 * Stall — a task that polls a status register until the host sets it.  On
 * costar's infinitely fast CPU the poll never lets time pass, so the host's
 * write never comes: costar stops the machine as stalled.  (Real firmware
 * would wait on the device's interrupt, or delay between reads.)
 */
#include "FreeRTOS.h"
#include "task.h"

#include "costar.h"

static void prvPoller( void * pvParameters )
{
    const int status = costar_dev( "status" );

    ( void ) pvParameters;

    while( costar_reg_read( status, 0 ) == 0 )
    {
    }

    costar_trace( "ready", costar_reg_read( status, 0 ) );
    vTaskDelete( NULL );
}

int main( void )
{
    xTaskCreate( prvPoller, "poller", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
    vTaskStartScheduler();
    return 0;
}
