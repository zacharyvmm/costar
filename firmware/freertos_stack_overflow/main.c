/*
 * Stack overflow — a task that copies a message into a buffer on its stack,
 * sized by the message, as firmware that trusts a length field does.  The
 * task has a 256-word stack: on a microcontroller a long message overflows
 * it, and costar stops the machine at the task's next costar call or switch.
 *
 * Register 0 of the "message" device is the message's length (0: none).
 * Register 1 set to 1 makes the task jump over its stack's canary: the
 * buffer is left untouched, and a call below it uses the stack instead.
 * That shows only when the task's whole stack is scanned (when it is
 * deleted).
 */
#include <string.h>

#include "FreeRTOS.h"
#include "task.h"

#include "costar.h"

static uint32_t prvChecksum( const volatile char * pcData, size_t xLength )
{
    uint32_t ulSum = 0;

    for( size_t i = 0; i < xLength; i++ )
    {
        ulSum += ( unsigned char ) pcData[ i ];
    }

    return ulSum;
}

static uint32_t prvCopy( uint32_t ulLength )
{
    char acMessage[ ulLength ];

    memset( acMessage, 'm', ulLength );
    return prvChecksum( acMessage, ulLength );
}

static __attribute__( ( noinline ) ) uint32_t prvBelow( void )
{
    volatile char acScratch[ 64 ];

    memset( ( char * ) acScratch, 's', sizeof( acScratch ) );
    return prvChecksum( acScratch, sizeof( acScratch ) );
}

static uint32_t prvJump( uint32_t ulLength )
{
    volatile char acMessage[ ulLength ];

    acMessage[ ulLength - 1 ] = 'm'; /* its shallow end */
    return acMessage[ ulLength - 1 ] + prvBelow();
}

static void prvParser( void * pvParameters )
{
    const int xMessage = costar_dev( "message" );
    const uint32_t ulLength = costar_reg_read( xMessage, 0 );

    ( void ) pvParameters;

    if( ulLength != 0 )
    {
        costar_trace( "checksum", costar_reg_read( xMessage, 1 ) == 1 ? prvJump( ulLength ) : prvCopy( ulLength ) );
    }

    costar_trace( "parsed", ulLength );
    vTaskDelete( NULL );
}

int main( void )
{
    xTaskCreate( prvParser, "parser", 256, NULL, 1, NULL );
    vTaskStartScheduler();
    return 0;
}
