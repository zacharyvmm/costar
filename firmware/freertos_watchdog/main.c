/*
 * Watchdog — a task feeds the watchdog every 25 ms (timeout 50 ms) between
 * rounds of its work.  Register 0 of the "control" device makes the work go
 * wrong: 1 deadlocks the task (it waits for a semaphore nobody gives), 2
 * fails an assertion, which stops the CPU.  Either way the watchdog is no
 * longer fed, and it reboots the machine.
 *
 * At boot the firmware reads how many times the watchdog has reset it
 * (traced as boot).  After a watchdog reset it runs in safe mode: it leaves
 * out the work that failed, so it keeps feeding the watchdog.
 */
#include "FreeRTOS.h"
#include "semphr.h"
#include "task.h"

#include "costar.h"

#define FEED_PERIOD_MS    25
#define TIMEOUT_US        50000

static int xWatchdog;
static int xControl;
static BaseType_t xSafeMode;

static void prvWork( void )
{
    switch( costar_reg_read( xControl, 0 ) )
    {
        case 1:
        {
            static StaticSemaphore_t xBuffer;
            SemaphoreHandle_t xNeverGiven = xSemaphoreCreateBinaryStatic( &xBuffer );

            costar_trace( "deadlocked", 0 );
            xSemaphoreTake( xNeverGiven, portMAX_DELAY );
            break;
        }

        case 2:
            configASSERT( pdFALSE );
            break;
    }
}

static void prvWorker( void * pvParameters )
{
    ( void ) pvParameters;

    for( ; ; )
    {
        if( xSafeMode == pdFALSE )
        {
            prvWork();
        }

        costar_reg_write( xWatchdog, COSTAR_WDT_FEED, 1 );
        vTaskDelay( pdMS_TO_TICKS( FEED_PERIOD_MS ) );
    }
}

int main( void )
{
    xWatchdog = costar_dev( "wdt" );
    xControl = costar_dev( "control" );

    const uint32_t ulResets = costar_reg_read( xWatchdog, COSTAR_WDT_RESETS );

    costar_trace( "boot", ulResets );
    xSafeMode = ulResets > 0;

    costar_reg_write( xWatchdog, COSTAR_WDT_TIMEOUT, TIMEOUT_US );
    costar_reg_write( xWatchdog, COSTAR_WDT_CTRL, 1 );

    xTaskCreate( prvWorker, "worker", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
    vTaskStartScheduler();
    return 0;
}
