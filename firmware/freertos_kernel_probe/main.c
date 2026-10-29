/*
 * Kernel probe — FreeRTOS behaviours that costar_legacy got wrong, checked by
 * tests/freertos_test.cpp.  A plain FreeRTOS application.
 *
 * Board: a "probe" register device whose register 0 selects the probe, and an
 * "irq" register device with an IRQ line, which the host raises by writing
 * to it.
 *
 *   1  tasks: runtime creation with a parameter, static allocation, and
 *      software timers
 *   2  interrupts: an IRQ raised inside a critical section runs at unmask and
 *      the task it wakes preempts; an IRQ between ticks runs on arrival
 *   3  a periodic timer interrupt on its own line, between ticks
 *   4  deleting a blocked task, then creating one in its place
 *   5  a failed configASSERT
 *   6  a task that returns from its function (a failed configASSERT too)
 */
#include "FreeRTOS.h"
#include "semphr.h"
#include "task.h"
#include "timers.h"

#include "costar.h"

#define PROBE_TIMER_IRQ    5

static TaskHandle_t xHigh;
static SemaphoreHandle_t xSemaphore;
static QueueHandle_t xQueue;

static uint32_t prvMicros( void )
{
    return ( uint32_t ) ( costar_now() / 1000 );
}

/*-------------------------------------------------- 1: tasks */

static uint32_t ulTimerFirings;
static StaticTask_t xStaticTcb;
static StackType_t uxStaticStack[ configMINIMAL_STACK_SIZE ];

static void prvChild( void * pvParameters )
{
    costar_trace( "child_param", *( uint32_t * ) pvParameters );
    vTaskDelete( NULL );
}

static void prvStatic( void * pvParameters )
{
    costar_trace( "static_param", ( uint32_t ) ( uintptr_t ) pvParameters );
    vTaskDelete( NULL );
}


static void prvTimer( TimerHandle_t xTimer )
{
    costar_trace( "timer", ++ulTimerFirings );

    if( ulTimerFirings == 3 )
    {
        xTimerStop( xTimer, 0 );
    }
}

static void prvParent( void * pvParameters )
{
    static uint32_t ulValue = 42;

    ( void ) pvParameters;

    /* Higher priority than this task: runs at once. */
    xTaskCreate( prvChild, "child", configMINIMAL_STACK_SIZE, &ulValue, 3, NULL );
    costar_trace( "parent_created", 1 );
    xTaskCreateStatic( prvStatic, "static", configMINIMAL_STACK_SIZE, ( void * ) 7, 3,
                       uxStaticStack, &xStaticTcb );
    costar_trace( "parent_created", 2 );

    xTimerStart( xTimerCreate( "timer", pdMS_TO_TICKS( 5 ), pdTRUE, NULL, prvTimer ), 0 );

    vTaskDelay( pdMS_TO_TICKS( 20 ) );
    costar_trace( "parent_done", ulTimerFirings );
    vTaskDelete( NULL );
}

/*-------------------------------------------------- 2: interrupts */

static void prvWakeHigh( void )
{
    BaseType_t xWoken = pdFALSE;

    vTaskNotifyGiveFromISR( xHigh, &xWoken );
    portYIELD_FROM_ISR( xWoken );
}

static void prvHigh( void * pvParameters )
{
    ( void ) pvParameters;

    for( ;; )
    {
        ulTaskNotifyTake( pdTRUE, portMAX_DELAY );
        costar_trace( "high_woken_us", prvMicros() );
    }
}

static void prvLow( void * pvParameters )
{
    ( void ) pvParameters;

    taskENTER_CRITICAL();
    costar_trace( "low_masked_us", prvMicros() );
    costar_cpu_poll( 2000000 ); /* 2 ms of work with interrupts masked */
    costar_trace( "low_unmasking_us", prvMicros() );
    taskEXIT_CRITICAL();
    costar_trace( "low_after_us", prvMicros() );
    vTaskDelete( NULL );
}

/*-------------------------------------------------- 3: timer interrupt */

#define PROBE_TIMER_PERIOD_NS    7500000ULL

static void prvTimerInterrupt( void )
{
    BaseType_t xWoken = pdFALSE;

    costar_timer_set( PROBE_TIMER_IRQ, costar_now() + PROBE_TIMER_PERIOD_NS );
    xSemaphoreGiveFromISR( xSemaphore, &xWoken );
    portYIELD_FROM_ISR( xWoken );
}

static void prvTimerWaiter( void * pvParameters )
{
    ( void ) pvParameters;

    for( uint32_t i = 0; i < 3; i++ )
    {
        xSemaphoreTake( xSemaphore, portMAX_DELAY );
        costar_trace( "timer_isr_us", prvMicros() );
    }

    costar_timer_set( PROBE_TIMER_IRQ, COSTAR_NEVER );
    vTaskDelete( NULL );
}

/*-------------------------------------------------- 4: deletion */

static void prvBlocked( void * pvParameters )
{
    uint32_t ulItem;

    ( void ) pvParameters;
    xQueueReceive( xQueue, &ulItem, portMAX_DELAY );
    costar_trace( "blocked_received", ulItem ); /* never: it is deleted first */
}

static void prvReplacement( void * pvParameters )
{
    uint32_t ulItem;

    ( void ) pvParameters;
    xQueueReceive( xQueue, &ulItem, portMAX_DELAY );
    costar_trace( "replacement_received", ulItem );
    vTaskDelete( NULL );
}

static void prvDeleter( void * pvParameters )
{
    TaskHandle_t xBlocked = ( TaskHandle_t ) pvParameters;
    const uint32_t ulItem = 9;

    vTaskDelay( pdMS_TO_TICKS( 1 ) );
    vTaskDelete( xBlocked );
    costar_trace( "deleted", 1 );
    xTaskCreate( prvReplacement, "replacement", configMINIMAL_STACK_SIZE, NULL, 3, NULL );
    xQueueSend( xQueue, &ulItem, 0 );
    costar_trace( "sent", ulItem );
    vTaskDelete( NULL );
}

/*-------------------------------------------------- 5: assertion */

static void prvAsserter( void * pvParameters )
{
    ( void ) pvParameters;
    vTaskDelay( pdMS_TO_TICKS( 3 ) );
    configASSERT( pvParameters != NULL );
    costar_trace( "after_assert", 1 ); /* never */
}

/*-------------------------------------------------- 6: returning */

static void prvReturner( void * pvParameters )
{
    ( void ) pvParameters;
    vTaskDelay( pdMS_TO_TICKS( 2 ) );
    costar_trace( "returning", 1 );
}

/*-----------------------------------------------------------*/

int main( void )
{
    const int xIrqDevice = costar_dev( "irq" );

    switch( costar_reg_read( costar_dev( "probe" ), 0 ) )
    {
        case 1:
            xTaskCreate( prvParent, "parent", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
            break;

        case 2:
            vPortSetInterruptHandler( ( unsigned ) costar_dev_irq( xIrqDevice ), prvWakeHigh );
            xTaskCreate( prvHigh, "high", configMINIMAL_STACK_SIZE, NULL, 3, &xHigh );
            xTaskCreate( prvLow, "low", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
            break;

        case 3:
            xSemaphore = xSemaphoreCreateBinary();
            vPortSetInterruptHandler( PROBE_TIMER_IRQ, prvTimerInterrupt );
            costar_timer_set( PROBE_TIMER_IRQ, PROBE_TIMER_PERIOD_NS );
            xTaskCreate( prvTimerWaiter, "waiter", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
            break;

        case 4:
        {
            TaskHandle_t xBlocked;
            xQueue = xQueueCreate( 1, sizeof( uint32_t ) );
            xTaskCreate( prvBlocked, "blocked", configMINIMAL_STACK_SIZE, NULL, 3, &xBlocked );
            xTaskCreate( prvDeleter, "deleter", configMINIMAL_STACK_SIZE, xBlocked, 1, NULL );
            break;
        }

        case 5:
            xTaskCreate( prvAsserter, "asserter", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
            break;

        case 6:
            xTaskCreate( prvReturner, "returner", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
            break;

        default:
            return 1;
    }

    vTaskStartScheduler();
    return 0;
}
