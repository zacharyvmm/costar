/*
 * Deadline — a control task released every 2 ms that must finish its work
 * before its next release.  Built with INSTRUMENT_KERNEL, so the work, and
 * the kernel's, takes time at the machine's speed (cpu.ns_per_block in a
 * scenario): on a fast CPU every period meets its deadline, on a slow one
 * they miss it.
 *
 * For each of 5 periods it traces deadline_met or deadline_missed, with the
 * period's number.
 */
#include "FreeRTOS.h"
#include "task.h"

#include "costar.h"

#define PERIOD_MS          2
#define WORK_ITERATIONS    50000

static volatile uint32_t ulState = 1;

/* A filter step: plain arithmetic, so its cost is basic blocks. */
static void prvWork( void )
{
    for( uint32_t i = 0; i < WORK_ITERATIONS; i++ )
    {
        ulState = ulState * 1103515245u + 12345u;
    }
}

static void prvControl( void * pvParameters )
{
    TickType_t xRelease = xTaskGetTickCount();

    ( void ) pvParameters;

    for( uint32_t ulPeriod = 1; ulPeriod <= 5; ulPeriod++ )
    {
        const costar_time xDeadline = ( costar_time ) ( xRelease + pdMS_TO_TICKS( PERIOD_MS ) ) * 1000000u /
                                      configTICK_RATE_HZ * 1000u;

        prvWork();
        costar_trace( costar_now() <= xDeadline ? "deadline_met" : "deadline_missed", ulPeriod );
        vTaskDelayUntil( &xRelease, pdMS_TO_TICKS( PERIOD_MS ) );
    }

    vTaskDelete( NULL );
}

int main( void )
{
    xTaskCreate( prvControl, "control", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
    vTaskStartScheduler();
    return 0;
}
