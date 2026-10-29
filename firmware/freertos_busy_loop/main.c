/*
 * Busy loop — a task that spins until another task sets a flag.  On costar's
 * infinitely fast CPU the spin only ends if the build is instrumented
 * (costar_add_freertos_image(... INSTRUMENT)): the tick interrupt then
 * preempts it and the higher-priority task gets to run.
 */
#include "FreeRTOS.h"
#include "task.h"

#include "costar.h"

static volatile int xReady;

static void prvSpinner( void * pvParameters )
{
    ( void ) pvParameters;

    while( !xReady )
    {
    }

    costar_trace( "spin_ended_us", ( uint32_t ) ( costar_now() / 1000 ) );
    vTaskDelete( NULL );
}

static void prvSetter( void * pvParameters )
{
    ( void ) pvParameters;

    vTaskDelay( pdMS_TO_TICKS( 5 ) );
    costar_trace( "setter_us", ( uint32_t ) ( costar_now() / 1000 ) );
    xReady = 1;
    vTaskDelete( NULL );
}

int main( void )
{
    xTaskCreate( prvSpinner, "spinner", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
    xTaskCreate( prvSetter, "setter", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
    vTaskStartScheduler();
    return 0;
}
