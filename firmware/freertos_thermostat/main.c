/*
 * Thermostat — a FreeRTOS application using registers and memory.
 *
 * Board: a "thermo" register device (register 0: temperature in 0.1 °C,
 * written by the world; register 1: heater on/off, written here) and a "log"
 * memory device.
 *
 * Every 100 ms: read the temperature, switch the heater on below 20.0 °C, and
 * append the reading to the log as a little-endian uint16.
 */
#include "FreeRTOS.h"
#include "task.h"

#include "costar.h"

#define SETPOINT 200 /* 20.0 °C */

static void prvControl( void * pvParameters )
{
    const int xThermo = costar_dev( "thermo" );
    const int xLog = costar_dev( "log" );
    uint32_t ulOffset = 0;

    ( void ) pvParameters;

    for( ;; )
    {
        const uint32_t ulTemperature = costar_reg_read( xThermo, 0 );
        const uint8_t ucSample[ 2 ] = { ( uint8_t ) ulTemperature, ( uint8_t ) ( ulTemperature >> 8 ) };

        costar_reg_write( xThermo, 1, ulTemperature < SETPOINT );

        if( costar_mem_write( xLog, ulOffset, ucSample, sizeof( ucSample ) ) == 0 )
        {
            ulOffset += sizeof( ucSample );
        }

        vTaskDelay( pdMS_TO_TICKS( 100 ) );
    }
}

int main( void )
{
    xTaskCreate( prvControl, "control", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
    vTaskStartScheduler();
    return 0;
}
