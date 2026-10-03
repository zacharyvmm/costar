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

/* The same pattern in reverse order: sim_create_task() first. */
void costar_test_legacy_reverse_boot( void )
{
    TaskHandle_t xHandle = NULL;
    sim_task_handle_t xSimHandle;

    xSimHandle = sim_create_task( "legacy", ( sim_task_entry_fn ) prvLegacyTask, NULL,
                                  configMINIMAL_STACK_SIZE, 1 );
    xTaskCreate( prvLegacyTask, "legacy", configMINIMAL_STACK_SIZE, NULL, 1, &xHandle );
    sim_bridge_register( xSimHandle, ( void * ) xHandle );
}

/* ── Simulator delay ABI ───────────────────────────────────────────
 * sim_task_delay_until() only suspended the fiber: FreeRTOS still saw the
 * task as ready and resumed it at once. */

static void prvAbiDelayer( void *pvParameters )
{
    ( void ) pvParameters;
    vTaskDelay( 5 );
    sim_trace_u32( "freertos_delay_done", ( uint32_t ) xTaskGetTickCount() );
    sim_task_delay_until( 12 );
    sim_trace_u32( "abi_delay_done", ( uint32_t ) xTaskGetTickCount() );
    vTaskDelete( NULL );
}

static void prvAbiBackground( void *pvParameters )
{
    ( void ) pvParameters;
    vTaskDelay( 8 );
    sim_trace_u32( "background_ran", ( uint32_t ) xTaskGetTickCount() );
    vTaskDelete( NULL );
}

void costar_test_abi_delay_boot( void )
{
    xTaskCreate( prvAbiDelayer, "delayer", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
    xTaskCreate( prvAbiBackground, "background", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}

/* ── Fault with interrupts masked ──────────────────────────────────
 * A task that faults inside a critical section must not leave the
 * machine's interrupts masked: every later yield would be pended forever. */

static void prvMaskedFaulter( void *pvParameters )
{
    volatile int iOk = 0;
    ( void ) pvParameters;
    taskENTER_CRITICAL();
    configASSERT( iOk );
    taskEXIT_CRITICAL();
}

static void prvAfterFault( void *pvParameters )
{
    ( void ) pvParameters;
    vTaskDelay( 1 );
    sim_trace_u32( "after_fault_ran", ( uint32_t ) xTaskGetTickCount() );
    vTaskDelete( NULL );
}

void costar_test_masked_fault_boot( void )
{
    xTaskCreate( prvMaskedFaulter, "faulter", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
    xTaskCreate( prvAfterFault, "after", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}

/* ── Reserved TLS slot ─────────────────────────────────────────────
 * The simulator keeps each task's fiber handle in the last TLS slot.  A
 * task that writes it fails configASSERT() and stops; the write is dropped,
 * so the task's fiber mapping stays intact.  Slot 0 is the application's. */

static void prvTlsWriter( void *pvParameters )
{
    static int iMarker;
    ( void ) pvParameters;
    vTaskSetThreadLocalStoragePointer( NULL, 0, &iMarker );
    sim_trace_u32( "tls_slot0_ok", pvTaskGetThreadLocalStoragePointer( NULL, 0 ) == &iMarker );
    vTaskSetThreadLocalStoragePointer( NULL, SIM_TLS_HANDLE_INDEX, &iMarker );
    sim_trace_u32( "tls_reserved_written", 1 );
    vTaskDelete( NULL );
}

static void prvTlsBystander( void *pvParameters )
{
    ( void ) pvParameters;
    vTaskDelay( 1 );
    sim_trace_u32( "tls_bystander_ran", 1 );
    vTaskDelete( NULL );
}

void costar_test_reserved_tls_boot( void )
{
    xTaskCreate( prvTlsWriter, "tls_writer", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
    xTaskCreate( prvTlsBystander, "bystander", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}

/* ── Host I/O waits ────────────────────────────────────────────────
 * A task blocked in sim_host_block_on_fd() stayed in FreeRTOS's ready
 * list, so FreeRTOS kept selecting it and the lower-priority task that
 * would send its data never ran. */

#ifndef _WIN32
#include <unistd.h>

static int iIoRecvFd;
static int iIoSendFd;

static void prvIoReceiver( void *pvParameters )
{
    char cByte;
    ( void ) pvParameters;
    while( read( iIoRecvFd, &cByte, 1 ) != 1 )
    {
        sim_host_block_on_fd( iIoRecvFd );
    }
    sim_trace_u32( "io_received", ( uint32_t ) cByte );
    vTaskDelete( NULL );
}

static void prvIoSender( void *pvParameters )
{
    ( void ) pvParameters;
    sim_trace_u32( "io_sent", ( uint32_t ) write( iIoSendFd, "x", 1 ) );
    vTaskDelete( NULL );
}

void costar_test_io_wait_boot( int iRecvFd, int iSendFd )
{
    iIoRecvFd = iRecvFd;
    iIoSendFd = iSendFd;
    xTaskCreate( prvIoReceiver, "receiver", configMINIMAL_STACK_SIZE, NULL, 3, NULL );
    xTaskCreate( prvIoSender, "sender", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}

/* Deleting a task blocked on a descriptor freed its TCB but left its wait
 * registered: later readiness called vTaskResume() on the freed TCB, and
 * the wait kept the machine alive.  With xReuse, "deleter" then creates a
 * task that suspends itself, most likely in the freed TCB's memory. */

static TaskHandle_t xDeletedIoWaiter;
static int iDeletedIoFd;
static BaseType_t xDeletedIoReuse;

static void prvDeletedIoWaiter( void *pvParameters )
{
    ( void ) pvParameters;
    sim_host_block_on_fd( iDeletedIoFd );
    sim_trace_u32( "io_deleted_waiter_resumed", 1 );
    vTaskDelete( NULL );
}

static void prvIoReuser( void *pvParameters )
{
    ( void ) pvParameters;
    sim_trace_u32( "io_reuser_started", 1 );
    vTaskSuspend( NULL );
    sim_trace_u32( "io_reuser_resumed", 1 );
    vTaskDelete( NULL );
}

static void prvIoDeleter( void *pvParameters )
{
    uintptr_t uxOldTcb = ( uintptr_t ) xDeletedIoWaiter;
    TaskHandle_t xReuser;
    ( void ) pvParameters;
    vTaskDelete( xDeletedIoWaiter );
    sim_trace_u32( "io_waiter_deleted", 1 );
    if( xDeletedIoReuse )
    {
        xTaskCreate( prvIoReuser, "reuser", configMINIMAL_STACK_SIZE, NULL, 2, &xReuser );
        sim_trace_u32( "io_tcb_reused", ( uintptr_t ) xReuser == uxOldTcb );
    }
    vTaskDelete( NULL );
}

void costar_test_io_delete_boot( int iFd, int iReuse )
{
    iDeletedIoFd = iFd;
    xDeletedIoReuse = iReuse;
    xTaskCreate( prvDeletedIoWaiter, "waiter", configMINIMAL_STACK_SIZE, NULL, 3, &xDeletedIoWaiter );
    xTaskCreate( prvIoDeleter, "deleter", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}
#endif

/* ── Busy tasks without time slicing ───────────────────────────────
 * Two equal-priority tasks that never block.  configUSE_TIME_SLICING is 0,
 * so the tick interrupts their exhausted budgets stand for must not rotate
 * between them: the task FreeRTOS selected first keeps the CPU. */

static void prvBusy( void *pvParameters )
{
    sim_trace_u32( ( const char * ) pvParameters, 1 );
    sim_budget_set_limit( 1 );
    for( ;; )
    {
        sim_budget_poll( NULL, __LINE__ );
    }
}

void costar_test_busy_no_slicing_boot( void )
{
    xTaskCreate( prvBusy, "busy_a", configMINIMAL_STACK_SIZE, ( void * ) "busy_a", 3, NULL );
    xTaskCreate( prvBusy, "busy_b", configMINIMAL_STACK_SIZE, ( void * ) "busy_b", 3, NULL );
}

/* ── Tasks readied between scheduling steps ────────────────────────
 * Three tasks block (suspended, waiting for a notification, waiting for a
 * semaphore) until the machine is quiescent; host code then readies them
 * one way each, after the scheduler ran. */

#include "semphr.h"

static TaskHandle_t xReadySuspended;
static TaskHandle_t xReadyNotified;
static SemaphoreHandle_t xReadySem;

static void prvReadySuspended( void *pvParameters )
{
    ( void ) pvParameters;
    vTaskSuspend( NULL );
    sim_trace_u32( "readied_by_resume", 1 );
    vTaskDelete( NULL );
}

static void prvReadyNotified( void *pvParameters )
{
    ( void ) pvParameters;
    ( void ) ulTaskNotifyTake( pdTRUE, portMAX_DELAY );
    sim_trace_u32( "readied_by_notify", 1 );
    vTaskDelete( NULL );
}

static void prvReadyGiven( void *pvParameters )
{
    ( void ) pvParameters;
    ( void ) xSemaphoreTake( xReadySem, portMAX_DELAY );
    sim_trace_u32( "readied_by_give", 1 );
    vTaskDelete( NULL );
}

void costar_test_ready_wake_boot( void )
{
    xReadySem = xSemaphoreCreateBinary();
    xTaskCreate( prvReadySuspended, "suspended", configMINIMAL_STACK_SIZE, NULL, 5, &xReadySuspended );
    xTaskCreate( prvReadyNotified, "notified", configMINIMAL_STACK_SIZE, NULL, 4, &xReadyNotified );
    xTaskCreate( prvReadyGiven, "given", configMINIMAL_STACK_SIZE, NULL, 3, NULL );
}

void costar_test_ready_wake_resume( void )
{
    vTaskResume( xReadySuspended );
}

void costar_test_ready_wake_notify( void )
{
    xTaskNotifyGive( xReadyNotified );
}

void costar_test_ready_wake_give( void )
{
    xSemaphoreGive( xReadySem );
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

/* The same, with an ISR that acknowledges its IRQ and then checks that
 * nothing else has arrived. */
static void prvTimerAckIsr( void )
{
    sim_irq_clear( 5 );
    sim_trace_u32( "pending_after_ack", sim_irq_pending() );
    prvTimerIsr();
}

void costar_test_timer_isr_ack_boot( void )
{
    costar_test_timer_isr_boot();
    sim_irq_set_handler( 5, prvTimerAckIsr );
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

/* Waiter for an IRQ 6 raised from outside the firmware (a World, a test). */
void costar_test_external_irq_boot( void )
{
    xTimerIsrSem = xSemaphoreCreateBinary();
    sim_irq_set_handler( 6, prvTimerIsr );
    xTaskCreate( prvTimerIsrWaiter, "waiter", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
}

/* ── Interrupts masked across a World step ─────────────────────────
 * The spinner uses up its budget at the first step's limit, so it is still
 * running when the next step starts; the test masks interrupts in between,
 * and the spinner unmasks them on resuming, at tick 1 (the tick its budget
 * used up is charged first), before the input's arrival.  IRQ input the
 * World staged for later in the new step must not be taken then. */

static void prvMaskedSpinner( void *pvParameters )
{
    ( void ) pvParameters;
    sim_budget_set_limit( 1 );
    sim_budget_poll( NULL, __LINE__ );
    sim_budget_set_limit( 1000000 );
    portENABLE_INTERRUPTS();
    sim_trace_u32( "spinner_unmasked", 1 );
    vTaskDelete( NULL );
}

void costar_test_masked_step_boot( void )
{
    costar_test_external_irq_boot();
    xTaskCreate( prvMaskedSpinner, "spinner", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}

/* ── Budget exhausted inside an ISR ────────────────────────────────
 * The budget's tick interrupt used to suspend the fiber in the middle of
 * the ISR, so the task the ISR woke ran before the ISR finished. */

static SemaphoreHandle_t xBudgetSem;

static void prvBudgetIsr( void )
{
    BaseType_t xWoken = pdFALSE;
    sim_trace_u32( "isr_start", 1 );
    xSemaphoreGiveFromISR( xBudgetSem, &xWoken );
    /* Exhaust the budget, as a long instrumented ISR would. */
    sim_budget_set_limit( 1 );
    sim_budget_poll( NULL, __LINE__ );
    sim_budget_set_limit( 1000000 );
    sim_trace_u32( "isr_end", 1 );
    portYIELD_FROM_ISR( xWoken );
}

static void prvBudgetHigh( void *pvParameters )
{
    ( void ) pvParameters;
    xSemaphoreTake( xBudgetSem, portMAX_DELAY );
    sim_trace_u32( "high_ran", 1 );
    vTaskDelete( NULL );
}

static void prvBudgetLow( void *pvParameters )
{
    ( void ) pvParameters;
    sim_irq_raise( 6 );
    sim_trace_u32( "low_after_isr", 1 );
    vTaskDelete( NULL );
}

void costar_test_isr_budget_boot( void )
{
    xBudgetSem = xSemaphoreCreateBinary();
    sim_irq_set_handler( 6, prvBudgetIsr );
    xTaskCreate( prvBudgetHigh, "high", configMINIMAL_STACK_SIZE, NULL, 3, NULL );
    xTaskCreate( prvBudgetLow, "low", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}

/* ── An ISR that masks interrupts ──────────────────────────────────
 * IRQs 7 and 8 are pending when interrupts are unmasked.  IRQ 7's ISR wakes
 * the high-priority task, requests a switch and calls
 * portDISABLE_INTERRUPTS().  IRQ 8's ISR and the switch must wait until
 * the firmware unmasks interrupts again. */

static SemaphoreHandle_t xMaskSem;

static void prvMaskingIsr( void )
{
    BaseType_t xWoken = pdFALSE;
    sim_trace_u32( "masking_isr", 1 );
    xSemaphoreGiveFromISR( xMaskSem, &xWoken );
    portYIELD_FROM_ISR( xWoken );
    portDISABLE_INTERRUPTS();
}

static void prvSecondIsr( void )
{
    sim_trace_u32( "second_isr", 1 );
}

static void prvMaskHigh( void *pvParameters )
{
    ( void ) pvParameters;
    xSemaphoreTake( xMaskSem, portMAX_DELAY );
    sim_trace_u32( "high_ran", 1 );
    vTaskDelete( NULL );
}

static void prvMaskLow( void *pvParameters )
{
    ( void ) pvParameters;
    taskENTER_CRITICAL();
    sim_irq_raise( 7 );
    sim_irq_raise( 8 );
    taskEXIT_CRITICAL();    /* IRQ 7 is taken and masks interrupts */
    sim_trace_u32( "low_still_masked", sim_irq_pending() );
    portENABLE_INTERRUPTS(); /* IRQ 8 is taken, then high preempts */
    sim_trace_u32( "low_after_enable", 1 );
    vTaskDelete( NULL );
}

void costar_test_isr_masks_boot( void )
{
    xMaskSem = xSemaphoreCreateBinary();
    sim_irq_set_handler( 7, prvMaskingIsr );
    sim_irq_set_handler( 8, prvSecondIsr );
    xTaskCreate( prvMaskHigh, "high", configMINIMAL_STACK_SIZE, NULL, 3, NULL );
    xTaskCreate( prvMaskLow, "low", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}

/* ── ISR taken between steps, while a low-priority task is running ──
 * The spinner uses up its budget, so the scheduler leaves it selected; an
 * IRQ then arrives in scheduler context and its ISR wakes the waiter
 * (priority 2) with portYIELD_FROM_ISR().  The waiter must run before the
 * spinner's next instruction. */

static void prvEntrySpinner( void *pvParameters )
{
    ( void ) pvParameters;
    sim_trace_u32( "spinner_started", 1 );
    sim_budget_set_limit( 1 );
    sim_budget_poll( NULL, __LINE__ );
    sim_budget_set_limit( 1000000 );
    sim_trace_u32( "spinner_resumed", 1 );
    vTaskDelete( NULL );
}

void costar_test_entry_isr_boot( void )
{
    costar_test_external_irq_boot();
    xTaskCreate( prvEntrySpinner, "spinner", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}

/* ── An ISR in scheduler context that masks interrupts ─────────────
 * The machine is idle.  IRQ 6's ISR resumes a suspended high-priority task,
 * requests a switch and leaves interrupts disabled.  The task must not run
 * until interrupts are unmasked again. */

static TaskHandle_t xMaskedResumeTask;

static void prvResumeAndMaskIsr( void )
{
    BaseType_t xYield = xTaskResumeFromISR( xMaskedResumeTask );
    sim_trace_u32( "resume_isr", 1 );
    portYIELD_FROM_ISR( xYield );
    portDISABLE_INTERRUPTS();
}

static void prvSuspendedHigh( void *pvParameters )
{
    ( void ) pvParameters;
    vTaskSuspend( NULL );
    sim_trace_u32( "high_resumed", 1 );
    vTaskDelete( NULL );
}

void costar_test_isr_masks_in_scheduler_boot( void )
{
    sim_irq_set_handler( 6, prvResumeAndMaskIsr );
    xTaskCreate( prvSuspendedHigh, "high", configMINIMAL_STACK_SIZE, NULL, 3, &xMaskedResumeTask );
}

/* ── A task's own yield after an ISR masked interrupts ─────────────
 * The yielder arms one-shot timer 0 (IRQ 6, created by the test harness)
 * to expire at once and yields; the engine takes the IRQ right after the
 * slice.  Its ISR readies a suspended high-priority task without
 * requesting a yield and leaves interrupts disabled.  The yield's switch
 * must wait for the unmask: the yielder continues first. */

static TaskHandle_t xYieldHigh;

static void prvReadyAndMaskIsr( void )
{
    ( void ) xTaskResumeFromISR( xYieldHigh );
    sim_trace_u32( "mask_isr", 1 );
    portDISABLE_INTERRUPTS();
}

static void prvYieldHigh( void *pvParameters )
{
    ( void ) pvParameters;
    vTaskSuspend( NULL );
    sim_trace_u32( "high_ran", 1 );
    vTaskDelete( NULL );
}

static void prvYielder( void *pvParameters )
{
    ( void ) pvParameters;
    sim_timer_arm( 0, 0 );
    taskYIELD();
    sim_trace_u32( "yielder_continued", 1 );
    portENABLE_INTERRUPTS(); /* the latched switch happens here */
    sim_trace_u32( "yielder_after_unmask", 1 );
    vTaskDelete( NULL );
}

void costar_test_masked_task_yield_boot( void )
{
    sim_irq_set_handler( 6, prvReadyAndMaskIsr );
    xTaskCreate( prvYieldHigh, "high", configMINIMAL_STACK_SIZE, NULL, 3, &xYieldHigh );
    xTaskCreate( prvYielder, "yielder", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}

/* ── A tick's switch suppressed by an ISR's mask ───────────────────
 * The high-priority task sleeps until tick 1.  The low-priority task arms
 * one-shot timer 0 (IRQ 6) for tick 1 and uses up its budget, so the tick
 * interrupt charged for it readies the high task; the timer's ISR, taken at
 * that tick, masks interrupts without requesting a yield.  The tick's
 * switch must happen as soon as the low task unmasks. */

static void prvMaskOnlyIsr( void )
{
    sim_trace_u32( "masking_isr", 1 );
    portDISABLE_INTERRUPTS();
}

static void prvTickHigh( void *pvParameters )
{
    ( void ) pvParameters;
    vTaskDelay( 1 );
    sim_trace_u32( "high_woke", 1 );
    vTaskDelete( NULL );
}

static void prvTickLow( void *pvParameters )
{
    ( void ) pvParameters;
    sim_timer_arm( 0, 1 );
    sim_budget_set_limit( 1 );
    sim_budget_poll( NULL, __LINE__ );
    sim_budget_set_limit( 1000000 );
    sim_trace_u32( "low_masked", 1 );
    portENABLE_INTERRUPTS(); /* the tick's switch happens here */
    sim_trace_u32( "low_after_unmask", 1 );
    vTaskDelete( NULL );
}

void costar_test_masked_tick_switch_boot( void )
{
    sim_irq_set_handler( 6, prvMaskOnlyIsr );
    xTaskCreate( prvTickHigh, "high", configMINIMAL_STACK_SIZE, NULL, 3, NULL );
    xTaskCreate( prvTickLow, "low", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}

/* ── World wake-up fixtures ────────────────────────────────────────
 * Firmware whose host side acts after the scheduler ran in a step. */

/* IRQ 6 gives the waiter's semaphore without requesting a yield: the
 * waiter is merely readied. */
static void prvGiveNoYieldIsr( void )
{
    sim_trace_u32( "timer_isr", 1 );
    xSemaphoreGiveFromISR( xTimerIsrSem, NULL );
}

void costar_test_external_irq_no_yield_boot( void )
{
    costar_test_external_irq_boot();
    sim_irq_set_handler( 6, prvGiveNoYieldIsr );
}

/* Arms one-shot timer 0 (created by the test harness) for tick 5, then
 * ends the scheduler. */
static void prvArmThenEnd( void *pvParameters )
{
    ( void ) pvParameters;
    sim_timer_arm( 0, 5 );
    sim_trace_u32( "ending", 1 );
    vTaskEndScheduler();
}

void costar_test_arm_then_end_boot( void )
{
    xTaskCreate( prvArmThenEnd, "ender", configMINIMAL_STACK_SIZE, NULL, 1, NULL );
}

/* ── A timer ISR that re-arms its timer with zero delay ────────────
 * One-shot timer 0 (IRQ 6, created by the test harness) fires at tick 1;
 * its ISR re-arms it to fire again at once, forever: an interrupt storm.
 * The scheduler must still return and time must still pass; a task that
 * sleeps until tick 3 still runs. */

static uint32_t ulStormIsrs;

static void prvStormIsr( void )
{
    ulStormIsrs++;
    sim_timer_arm( 0, 0 );
}

static void prvStormSleeper( void *pvParameters )
{
    ( void ) pvParameters;
    vTaskDelay( 3 );
    sim_trace_u32( "slept_through_storm", ( uint32_t ) xTaskGetTickCount() );
    vTaskDelete( NULL );
}

void costar_test_timer_storm_boot( void )
{
    ulStormIsrs = 0;
    sim_irq_set_handler( 6, prvStormIsr );
    xTaskCreate( prvStormSleeper, "sleeper", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
    sim_timer_arm( 0, 1 );
}

uint32_t costar_test_timer_storm_isrs( void )
{
    return ulStormIsrs;
}

/* ── Peripheral callbacks scheduled by ISRs ────────────────────────
 * IRQ 6's ISR schedules a peripheral callback (sim_schedule_event). */

static void prvPeripheralCallback( void )
{
    sim_trace_u32( "peripheral_callback", ( uint32_t ) sim_now_ticks() );
}

static void prvScheduleLaterIsr( void )
{
    sim_schedule_event( sim_now_ticks() + 5, prvPeripheralCallback );
}

/* An idle machine whose IRQ 6 schedules a callback 5 ticks later. */
void costar_test_isr_schedules_event_boot( void )
{
    costar_test_external_irq_boot();
    sim_irq_set_handler( 6, prvScheduleLaterIsr );
}

/* A callback raises IRQ 6, whose ISR schedules the callback again for the
 * current tick, forever: a storm through the peripheral event queue.  A
 * task sleeping until tick 3 must still run. */

static uint32_t ulCallbackStorm;

static void prvStormCallback( void )
{
    ulCallbackStorm++;
    sim_irq_raise( 6 );
}

static void prvRescheduleNowIsr( void )
{
    sim_schedule_event( sim_now_ticks(), prvStormCallback );
}

void costar_test_callback_storm_boot( void )
{
    ulCallbackStorm = 0;
    sim_irq_set_handler( 6, prvRescheduleNowIsr );
    xTaskCreate( prvStormSleeper, "sleeper", configMINIMAL_STACK_SIZE, NULL, 2, NULL );
    sim_schedule_event( 1, prvStormCallback );
}

uint32_t costar_test_callback_storm_count( void )
{
    return ulCallbackStorm;
}

/* ── A finite self-retriggering IRQ ────────────────────────────────
 * IRQ 6's ISR raises IRQ 6 again until it has run 50000 times: more than
 * one delivery (1024 IRQs) or one tick's storm bound can take.  Every one
 * of them must still run. */

static uint32_t ulRetriggers;

static void prvRetriggerIsr( void )
{
    if( ++ulRetriggers < 50000u )
    {
        sim_irq_raise( 6 );
    }
}

void costar_test_retrigger_boot( void )
{
    ulRetriggers = 0;
    costar_test_external_irq_boot();
    sim_irq_set_handler( 6, prvRetriggerIsr );
}

uint32_t costar_test_retrigger_count( void )
{
    return ulRetriggers;
}

/* ── A FreeRTOS task running a host-provided body ──────────────────
 * The storm-halt matrix (crates/sim-ffi/tests/storm_halt_matrix.rs) runs
 * the same task bodies on every backend; on FreeRTOS each one is a plain
 * FreeRTOS task.  The body never returns while the machine runs. */

static void prvHostBody( void *pvParameters )
{
    ( ( void ( * )( void ) ) pvParameters )();
    vTaskDelete( NULL );
}

void costar_test_spawn_task( const char *pcName, void ( *pxBody )( void ), uint32_t ulPriority )
{
    xTaskCreate( prvHostBody, pcName, configMINIMAL_STACK_SIZE, ( void * ) pxBody,
                 ( UBaseType_t ) ulPriority, NULL );
}
