/*
 * sched_regressions.c — FreeRTOS scheduling regression firmware.
 *
 * Each costar_test_*_boot() function creates the tasks for one scenario,
 * exactly as unmodified firmware would (plain xTaskCreate, no simulator
 * calls).  The Rust tests in crates/sim-ffi/tests/freertos_scheduling.rs
 * boot one scenario per Simulator, step the scheduler and check the
 * sim_trace_u32 records.
 */

#include "FreeRTOS.h"
#include "task.h"
#include "queue.h"
#include "timers.h"
#include "sim_abi.h"

/* ── Runtime task creation ─────────────────────────────────────────
 * A task that creates another task after the scheduler started.  The
 * child used to never get a fiber and silently never ran. */

static void prvRuntimeChild( void *pvParameters )
{
    ( void ) pvParameters;
    sim_trace_u32( "child_ran", 1 );
    vTaskDelete( NULL );
}

static void prvRuntimeParent( void *pvParameters )
{
    ( void ) pvParameters;
    vTaskDelay( 2 );
    xTaskCreate( prvRuntimeChild, "child", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
    sim_trace_u32( "parent_created_child", 1 );
    vTaskDelete( NULL );
}

void costar_test_runtime_create_boot( void )
{
    xTaskCreate( prvRuntimeParent, "parent", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}

/* ── Blocking queue + preemption on send ───────────────────────────
 * A high-priority consumer blocks forever on a queue; a low-priority
 * producer sends one item per tick.  FreeRTOS must switch to the consumer
 * inside xQueueSend(), before the producer's next statement. */

static QueueHandle_t xBlockingQueue;

static void prvConsumer( void *pvParameters )
{
    uint32_t ulValue;
    ( void ) pvParameters;

    for( ;; )
    {
        if( xQueueReceive( xBlockingQueue, &ulValue, portMAX_DELAY ) == pdPASS )
        {
            sim_trace_u32( "rx", ulValue );
        }
    }
}

static void prvProducer( void *pvParameters )
{
    uint32_t ulValue;
    ( void ) pvParameters;

    for( ulValue = 1; ulValue <= 5; ulValue++ )
    {
        xQueueSend( xBlockingQueue, &ulValue, portMAX_DELAY );
        sim_trace_u32( "sent", ulValue );
        vTaskDelay( 1 );
    }

    vTaskDelete( NULL );
}

void costar_test_blocking_queue_boot( void )
{
    xBlockingQueue = xQueueCreate( 2, sizeof( uint32_t ) );
    xTaskCreate( prvConsumer, "consumer", configMINIMAL_STACK_SIZE, NULL, 3, NULL );
    xTaskCreate( prvProducer, "producer", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}

/* ── Software timers ───────────────────────────────────────────────
 * The timer daemon task used to be skipped at creation, so callbacks
 * never fired. */

static void prvTimerCallback( TimerHandle_t xTimer )
{
    static uint32_t ulFires = 0;
    ( void ) xTimer;
    sim_trace_u32( "timer_fired", ++ulFires );
}

void costar_test_software_timer_boot( void )
{
    TimerHandle_t xTimer = xTimerCreate( "t10", 10, pdTRUE, NULL, prvTimerCallback );
    xTimerStart( xTimer, 0 );
}

/* ── Task parameters ───────────────────────────────────────────────
 * The entry point and parameter used to be read from the wrong slots of
 * a 32-bit stack frame: a non-NULL parameter was called as a function. */

static uint32_t ulParamCookie = 0xC0FFEEu;

static void prvParamTask( void *pvParameters )
{
    sim_trace_u32( "param_ok", ( pvParameters == &ulParamCookie ) ? 1u : 0u );
    sim_trace_u32( "param_value", *( uint32_t * ) pvParameters );
    vTaskDelete( NULL );
}

void costar_test_task_param_boot( void )
{
    xTaskCreate( prvParamTask, "param", configMINIMAL_STACK_SIZE, &ulParamCookie, 1, NULL );
}

/* ── Task function returns ─────────────────────────────────────────
 * Returning from a task function is illegal in FreeRTOS; the simulator
 * deletes the task and keeps the others running. */

static void prvReturningTask( void *pvParameters )
{
    ( void ) pvParameters;
    sim_trace_u32( "returning", 1 );
}

static void prvSurvivor( void *pvParameters )
{
    ( void ) pvParameters;
    vTaskDelay( 3 );
    sim_trace_u32( "survivor_ran", 1 );
    vTaskDelete( NULL );
}

void costar_test_task_return_boot( void )
{
    xTaskCreate( prvReturningTask, "returns", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
    xTaskCreate( prvSurvivor, "survivor", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}

/* ── Static allocation ─────────────────────────────────────────────
 * The TCB used to be larger than StaticTask_t, so xTaskCreateStatic
 * overflowed its buffer. */

static StaticTask_t xStaticTcb;
static StackType_t uxStaticStack[ configMINIMAL_STACK_SIZE ];
static volatile uint32_t ulGuardAfterTcb = 0xA5A5A5A5u;

static void prvStaticTask( void *pvParameters )
{
    ( void ) pvParameters;
    sim_trace_u32( "static_ran", 1 );
    sim_trace_u32( "static_guard_intact", ulGuardAfterTcb == 0xA5A5A5A5u );
    vTaskDelete( NULL );
}

void costar_test_static_task_boot( void )
{
    xTaskCreateStatic( prvStaticTask, "static", configMINIMAL_STACK_SIZE, NULL, 1,
                       uxStaticStack, &xStaticTcb );
}

/* ── Legacy creation pattern ───────────────────────────────────────
 * Older firmware created each task twice: xTaskCreate() for FreeRTOS and
 * sim_create_task() + sim_bridge_register() for the simulator.  That must
 * still yield exactly one schedulable task. */

static void prvLegacyTask( void *pvParameters )
{
    ( void ) pvParameters;
    vTaskDelay( 1 );
    sim_trace_u32( "legacy_ran", 1 );
    vTaskDelete( NULL );
}

void costar_test_legacy_pattern_boot( void )
{
    TaskHandle_t xHandle = NULL;
    sim_task_handle_t xSimHandle;

    xTaskCreate( prvLegacyTask, "legacy", configMINIMAL_STACK_SIZE, NULL, 1, &xHandle );
    xSimHandle = sim_create_task( "legacy", ( sim_task_entry_fn ) prvLegacyTask, NULL,
                                  configMINIMAL_STACK_SIZE, 1 );
    sim_bridge_register( xSimHandle, ( void * ) xHandle );
}

/* ── Interrupts ────────────────────────────────────────────────────
 * Virtual IRQs used to be recorded in the trace and dropped: no ISR ever
 * ran, and a virtual timer's expiry did not wake a blocked system. */

#include "semphr.h"

static SemaphoreHandle_t xTimerIsrSem;

static void prvTimerIsr( void )
{
    BaseType_t xWoken = pdFALSE;
    sim_trace_u32( "timer_isr", 1 );
    xSemaphoreGiveFromISR( xTimerIsrSem, &xWoken );
    portYIELD_FROM_ISR( xWoken );
}

static void prvTimerIsrWaiter( void *pvParameters )
{
    uint32_t ulWakes = 0;
    ( void ) pvParameters;

    for( ;; )
    {
        if( xSemaphoreTake( xTimerIsrSem, portMAX_DELAY ) == pdPASS )
        {
            sim_trace_u32( "isr_woke_task", ++ulWakes );
        }
    }
}

/* Expects virtual timer 0 on IRQ 5 (created by the test harness). */
void costar_test_timer_isr_boot( void )
{
    xTimerIsrSem = xSemaphoreCreateBinary();
    sim_irq_set_handler( 5, prvTimerIsr );
    xTaskCreate( prvTimerIsrWaiter, "waiter", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
    sim_timer_arm( 0, 7 );
}

static SemaphoreHandle_t xPreemptSem;

static void prvSoftIsr( void )
{
    BaseType_t xWoken = pdFALSE;
    sim_trace_u32( "soft_isr", 1 );
    xSemaphoreGiveFromISR( xPreemptSem, &xWoken );
    portYIELD_FROM_ISR( xWoken );
}

static void prvHighWaiter( void *pvParameters )
{
    ( void ) pvParameters;
    xSemaphoreTake( xPreemptSem, portMAX_DELAY );
    sim_trace_u32( "high_ran", 1 );
    vTaskDelete( NULL );
}

static void prvLowRaiser( void *pvParameters )
{
    ( void ) pvParameters;
    vTaskDelay( 1 );

    taskENTER_CRITICAL();
    sim_irq_raise( 6 );
    sim_trace_u32( "raised_while_masked", 1 ); /* the ISR must not run yet */
    taskEXIT_CRITICAL();                       /* ISR runs, high preempts */

    sim_trace_u32( "low_after_unmask", 1 );
    vTaskDelete( NULL );
}

void costar_test_isr_preemption_boot( void )
{
    xPreemptSem = xSemaphoreCreateBinary();
    sim_irq_set_handler( 6, prvSoftIsr );
    xTaskCreate( prvHighWaiter, "high", configMINIMAL_STACK_SIZE, NULL, 3, NULL );
    xTaskCreate( prvLowRaiser, "low", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}
