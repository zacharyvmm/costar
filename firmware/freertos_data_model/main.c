/*
 * The data model of a FreeRTOS image: the C types (data_model.c), and the
 * port's types.  On Cortex-M (the GCC/ARM_CM* ports), StackType_t,
 * BaseType_t and UBaseType_t are 4 bytes, as they are here in an i686 build.
 */
#include "FreeRTOS.h"
#include "task.h"

#include "costar.h"
#include "data_model.h"

static void prvReport( void * pvParameters )
{
    ( void ) pvParameters;
    data_model_report( costar_trace );
    costar_trace( "sizeof_StackType_t", sizeof( StackType_t ) );
    costar_trace( "sizeof_BaseType_t", sizeof( BaseType_t ) );
    costar_trace( "sizeof_UBaseType_t", sizeof( UBaseType_t ) );
    costar_trace( "sizeof_TickType_t", sizeof( TickType_t ) );
    costar_cpu_stop( 0 );
}

int main( void )
{
    xTaskCreate( prvReport, "report", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
    vTaskStartScheduler();
    return 0;
}
