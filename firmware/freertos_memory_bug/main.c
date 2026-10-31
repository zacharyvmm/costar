/*
 * Memory bugs — a task that overruns a buffer, for sanitizer builds to catch.
 * Register 0 of the "bug" device picks the buffer: 1 overruns one from
 * pvPortMalloc(), 2 one on the task's stack.
 */
#include <string.h>

#include "FreeRTOS.h"
#include "task.h"

#include "costar.h"

static void prvOverrun( char * pcBuffer, size_t xLength )
{
    /* Through a volatile pointer, so the compiler can't see the overrun. */
    char * volatile pcEnd = pcBuffer + xLength;

    *pcEnd = 'x';
}

static void prvBuggy( void * pvParameters )
{
    char acOnStack[ 16 ];

    ( void ) pvParameters;

    switch( costar_reg_read( costar_dev( "bug" ), 0 ) )
    {
        case 1:
            prvOverrun( pvPortMalloc( 16 ), 16 );
            break;

        case 2:
            prvOverrun( acOnStack, sizeof( acOnStack ) );
            break;
    }

    costar_trace( "survived", 0 );
    vTaskDelete( NULL );
}

int main( void )
{
    xTaskCreate( prvBuggy, "buggy", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
    vTaskStartScheduler();
    return 0;
}
