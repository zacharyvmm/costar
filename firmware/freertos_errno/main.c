/*
 * errno per task.  Task A sets errno, then creates a higher-priority task B,
 * which runs at once, sets its own errno, and blocks.  Back in A, errno must
 * still be A's: errno is per thread on real targets (and on native_sim).
 */
#include <errno.h>

#include "FreeRTOS.h"
#include "task.h"

#include "costar.h"

static void prvB( void * pvParameters )
{
    ( void ) pvParameters;
    errno = 22;
    vTaskDelay( 1 );
    costar_trace( "b_errno", ( uint32_t ) errno );
    vTaskDelete( NULL );
}

static void prvA( void * pvParameters )
{
    ( void ) pvParameters;
    errno = 11;
    xTaskCreate( prvB, "b", configMINIMAL_STACK_SIZE, NULL, 3, NULL );
    costar_trace( "a_errno", ( uint32_t ) errno );
    vTaskDelete( NULL );
}

int main( void )
{
    xTaskCreate( prvA, "a", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
    vTaskStartScheduler();
    return 0;
}
