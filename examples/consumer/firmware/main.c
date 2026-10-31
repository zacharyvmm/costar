/*
 * Mirror — wakes on its "sensor" device's interrupt and writes twice the
 * reading to the sensor's register 1.
 */
#include "FreeRTOS.h"
#include "task.h"

#include "costar.h"

static TaskHandle_t xMirror;

static void prvSensorInterrupt( void )
{
    BaseType_t xWoken = pdFALSE;

    vTaskNotifyGiveFromISR( xMirror, &xWoken );
    portYIELD_FROM_ISR( xWoken );
}

static void prvMirror( void * pvParameters )
{
    const int xSensor = costar_dev( "sensor" );

    ( void ) pvParameters;

    for( ;; )
    {
        ulTaskNotifyTake( pdTRUE, portMAX_DELAY );
        costar_reg_write( xSensor, 1, 2 * costar_reg_read( xSensor, 0 ) );
    }
}

int main( void )
{
    vPortSetInterruptHandler( ( unsigned ) costar_dev_irq( costar_dev( "sensor" ) ), prvSensorInterrupt );
    xTaskCreate( prvMirror, "mirror", configMINIMAL_STACK_SIZE, NULL, 2, &xMirror );
    vTaskStartScheduler();
    return 0;
}
