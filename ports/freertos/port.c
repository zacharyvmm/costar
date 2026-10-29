/*
 * FreeRTOS port for costar — port.c
 *
 * FreeRTOS makes every scheduling decision; this file only maps the port
 * contract onto costar's virtual CPU (include/costar.h):
 *
 *   task              → costar thread (its handle lives at the top of the
 *                       task's stack, as in the upstream Posix port)
 *   portYIELD         → vTaskSwitchContext(), then switch to the chosen task.
 *                       Inside a critical section the switch is deferred to
 *                       the end of the section, like PendSV on Cortex-M.
 *   tick interrupt    → a one-shot costar timer, re-armed by the tick ISR
 *   tickless idle     → halt the CPU until the next timeout or interrupt
 */
#include "FreeRTOS.h"
#include "task.h"

#if ( configNUMBER_OF_CORES != 1 )
    #error "The costar port is single-core."
#endif

#if ( INCLUDE_xTaskGetIdleTaskHandle != 1 )
    #error "The costar port needs INCLUDE_xTaskGetIdleTaskHandle 1."
#endif

#ifndef configCOSTAR_TICK_IRQ
    #define configCOSTAR_TICK_IRQ    0
#endif

#define portTICK_NS    ( ( costar_time ) 1000000000ULL / configTICK_RATE_HZ )

typedef struct PortThread
{
    costar_thread xThread;
} PortThread_t;

static UBaseType_t uxCriticalNesting;
static unsigned uxCriticalKey;
static BaseType_t xSwitchPending;          /* yield requested inside a critical section */
static BaseType_t xSwitchRequestedByISR;
static uint32_t ulSwitchCount;             /* number of real thread switches */
static BaseType_t xIdleSlept;              /* the last idle iteration did a tickless sleep */
static costar_time xNextTickAt;            /* time of the next tick interrupt */
static void ( * pxInterruptHandlers[ COSTAR_IRQ_LINES ] )( void );

/*-----------------------------------------------------------*/

static PortThread_t * prvThreadOf( TaskHandle_t xTask )
{
    /* pxTopOfStack is the first member of every TCB. */
    StackType_t * pxTopOfStack = *( StackType_t ** ) xTask;

    return ( PortThread_t * ) ( pxTopOfStack + 1 );
}

StackType_t * pxPortInitialiseStack( StackType_t * pxTopOfStack,
                                     StackType_t * pxEndOfStack,
                                     TaskFunction_t pxCode,
                                     void * pvParameters )
{
    PortThread_t * pxThread = ( PortThread_t * ) ( pxTopOfStack + 1 ) - 1;

    pxTopOfStack = ( StackType_t * ) pxThread - 1;
    configASSERT( pxTopOfStack > pxEndOfStack );

    pxThread->xThread = costar_thread_new( pxCode, pvParameters,
                                           ( size_t ) ( pxTopOfStack - pxEndOfStack ) * sizeof( StackType_t ) );
    return pxTopOfStack;
}

void vPortCleanUpTCB( void * pxTCB )
{
    costar_thread_free( prvThreadOf( ( TaskHandle_t ) pxTCB )->xThread );
}

/*-----------------------------------------------------------*/

void vPortYield( void )
{
    if( uxCriticalNesting > 0 )
    {
        xSwitchPending = pdTRUE;
        return;
    }

    xSwitchPending = pdFALSE;
    vPortEnterCritical();
    TaskHandle_t xPrevious = xTaskGetCurrentTaskHandle();
    vTaskSwitchContext();
    vPortExitCritical();

    /* Re-read the current task: an interrupt delivered when the critical
     * section ended may already have switched away and back. */
    TaskHandle_t xNext = xTaskGetCurrentTaskHandle();

    if( xNext != xPrevious )
    {
        ulSwitchCount++;
    }

    costar_thread_switch( prvThreadOf( xNext )->xThread );
}

void vPortYieldFromISR( BaseType_t xSwitchRequired )
{
    if( xSwitchRequired != pdFALSE )
    {
        xSwitchRequestedByISR = pdTRUE;
    }
}

void vPortEnterCritical( void )
{
    unsigned uxKey = costar_irq_lock();

    if( uxCriticalNesting++ == 0 )
    {
        uxCriticalKey = uxKey;
    }
}

void vPortExitCritical( void )
{
    configASSERT( uxCriticalNesting > 0 );

    if( --uxCriticalNesting == 0 )
    {
        costar_irq_unlock( uxCriticalKey );

        if( xSwitchPending != pdFALSE )
        {
            vPortYield();
        }
    }
}

/*-----------------------------------------------------------*/

void vPortSetInterruptHandler( unsigned uxIrq,
                               void ( * pxHandler )( void ) )
{
    configASSERT( uxIrq < COSTAR_IRQ_LINES && uxIrq != configCOSTAR_TICK_IRQ );
    pxInterruptHandlers[ uxIrq ] = pxHandler;
}

static void prvTickInterrupt( void )
{
    /* Count every tick boundary that has passed (several if the CPU was busy). */
    while( xNextTickAt <= costar_now() )
    {
        xNextTickAt += portTICK_NS;
        portYIELD_FROM_ISR( xTaskIncrementTick() );
    }

    costar_timer_set( configCOSTAR_TICK_IRQ, xNextTickAt );
}

/* costar calls this for each pending IRQ, then calls vPortYield() if any
 * handler returned non-zero. */
static int prvDispatchInterrupt( unsigned uxIrq )
{
    xSwitchRequestedByISR = pdFALSE;

    if( uxIrq == configCOSTAR_TICK_IRQ )
    {
        prvTickInterrupt();
    }
    else if( pxInterruptHandlers[ uxIrq ] != NULL )
    {
        pxInterruptHandlers[ uxIrq ]();
    }

    return xSwitchRequestedByISR != pdFALSE;
}

/*-----------------------------------------------------------*/

BaseType_t xPortStartScheduler( void )
{
    costar_cpu_init( prvDispatchInterrupt, vPortYield );

    xNextTickAt = ( costar_now() / portTICK_NS + 1 ) * portTICK_NS;
    costar_timer_set( configCOSTAR_TICK_IRQ, xNextTickAt );

    /* vTaskStartScheduler() disabled interrupts; tasks start with them on. */
    costar_irq_unlock( COSTAR_IRQS_ENABLED );

    /* Run the task FreeRTOS selected.  The calling (boot) thread is never
     * resumed. */
    costar_thread_switch( prvThreadOf( xTaskGetCurrentTaskHandle() )->xThread );
    return pdFALSE;
}

void vPortEndScheduler( void )
{
}

/*-----------------------------------------------------------*/

void vPortTaskLoopIteration( void )
{
    if( xTaskGetCurrentTaskHandle() != xTaskGetIdleTaskHandle() )
    {
        return; /* The timer task's loop; it blocks on its own. */
    }

    if( xIdleSlept != pdFALSE )
    {
        /* The kernel's tickless idle just slept; let it decide again. */
        xIdleSlept = pdFALSE;
        return;
    }

    /* Let other idle-priority tasks run first.  If FreeRTOS keeps the idle
     * task, nothing can change until an interrupt arrives: wait for one. */
    uint32_t ulSwitchesBefore = ulSwitchCount;
    taskYIELD();

    if( ulSwitchCount == ulSwitchesBefore )
    {
        costar_cpu_halt();
    }
}

#if ( configUSE_TICKLESS_IDLE != 0 )

    void vPortSuppressTicksAndSleep( TickType_t xExpectedIdleTime )
    {
        vPortEnterCritical();

        if( eTaskConfirmSleepModeStatus() == eAbortSleep )
        {
            vPortExitCritical();
            return;
        }

        /* Sleep until tick (last + xExpectedIdleTime), or until an interrupt
         * if no task is waiting with a timeout. */
        const costar_time xLastTickAt = xNextTickAt - portTICK_NS;
        const BaseType_t xNoTimeout = ( xExpectedIdleTime >= portMAX_DELAY - xTaskGetTickCount() );
        costar_timer_set( configCOSTAR_TICK_IRQ,
                          xNoTimeout ? COSTAR_NEVER : xLastTickAt + xExpectedIdleTime * portTICK_NS );

        costar_cpu_halt();
        xIdleSlept = pdTRUE;

        const TickType_t xElapsed = ( TickType_t ) ( ( costar_now() - xLastTickAt ) / portTICK_NS );

        if( ( xNoTimeout == pdFALSE ) && ( xElapsed >= xExpectedIdleTime ) )
        {
            /* The tick timer woke us; its pending interrupt counts the last tick. */
            vTaskStepTick( xExpectedIdleTime - 1 );
            xNextTickAt = xLastTickAt + xExpectedIdleTime * portTICK_NS;
        }
        else
        {
            /* Another interrupt woke us early. */
            vTaskStepTick( xElapsed );
            xNextTickAt = xLastTickAt + ( ( costar_time ) xElapsed + 1 ) * portTICK_NS;
            costar_timer_set( configCOSTAR_TICK_IRQ, xNextTickAt );
        }

        vPortExitCritical(); /* Runs the interrupt that woke us. */
    }

#endif /* configUSE_TICKLESS_IDLE */
