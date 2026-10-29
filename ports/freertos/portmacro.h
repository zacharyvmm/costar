/*
 * FreeRTOS port for costar — portmacro.h
 *
 * Same shape as the upstream GCC/Posix port, with host threads and signals
 * replaced by costar threads and the virtual tick timer.  See port.c.
 *
 * Requirements on the application's FreeRTOSConfig.h:
 *   - configNUMBER_OF_CORES 1 (or undefined).
 *   - configUSE_TICKLESS_IDLE 1 is recommended: idle periods then skip
 *     straight to the next timeout instead of waking on every tick.
 *   - configCOSTAR_TICK_IRQ (optional, default 0): the IRQ line used for the
 *     tick.  Use vPortSetInterruptHandler() for the other lines.
 */
#ifndef PORTMACRO_H
#define PORTMACRO_H

#include <stdint.h>

#include "costar.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ── Types ─────────────────────────────────────────────────────────── */

#define portCHAR                 char
#define portFLOAT                float
#define portDOUBLE               double
#define portLONG                 long
#define portSHORT                short
#define portSTACK_TYPE           uintptr_t
#define portBASE_TYPE            long
#define portPOINTER_SIZE_TYPE    uintptr_t

typedef portSTACK_TYPE StackType_t;
typedef long           BaseType_t;
typedef unsigned long  UBaseType_t;

#if ( configTICK_TYPE_WIDTH_IN_BITS == TICK_TYPE_WIDTH_64_BITS )
    typedef uint64_t TickType_t;
    #define portMAX_DELAY    ( ( TickType_t ) UINT64_MAX )
#elif ( configTICK_TYPE_WIDTH_IN_BITS == TICK_TYPE_WIDTH_32_BITS )
    typedef uint32_t TickType_t;
    #define portMAX_DELAY    ( ( TickType_t ) UINT32_MAX )
#else
    #error "The costar port supports 32- and 64-bit ticks."
#endif

/* Only one thread runs at a time, so tick reads are never torn. */
#define portTICK_TYPE_IS_ATOMIC    1

/* ── Architecture ──────────────────────────────────────────────────── */

#define portSTACK_GROWTH                   ( -1 )
#define portHAS_STACK_OVERFLOW_CHECKING    1
#define portBYTE_ALIGNMENT                 16
#define portTICK_PERIOD_MS                 ( ( TickType_t ) 1000 / configTICK_RATE_HZ )
#define portNOP()
#define portMEMORY_BARRIER()               __asm volatile ( "" ::: "memory" )

/* ── Scheduling ────────────────────────────────────────────────────── */

void vPortYield( void );
void vPortYieldFromISR( BaseType_t xSwitchRequired );

#define portYIELD()                  vPortYield()
#define portEND_SWITCHING_ISR( x )   vPortYieldFromISR( x )
#define portYIELD_FROM_ISR( x )      vPortYieldFromISR( x )

/* ── Interrupts and critical sections ──────────────────────────────── */

void vPortEnterCritical( void );
void vPortExitCritical( void );

#define portDISABLE_INTERRUPTS()                  ( ( void ) costar_irq_lock() )
#define portENABLE_INTERRUPTS()                   costar_irq_unlock( COSTAR_IRQS_ENABLED )
#define portSET_INTERRUPT_MASK_FROM_ISR()         ( ( UBaseType_t ) costar_irq_lock() )
#define portCLEAR_INTERRUPT_MASK_FROM_ISR( x )    costar_irq_unlock( ( unsigned ) ( x ) )
#define portENTER_CRITICAL()                      vPortEnterCritical()
#define portEXIT_CRITICAL()                       vPortExitCritical()

/* Install the handler for an IRQ line.  Handlers are plain functions that
 * use the ...FromISR APIs and portYIELD_FROM_ISR(), as on real hardware. */
void vPortSetInterruptHandler( unsigned uxIrq, void ( * pxHandler )( void ) );

/* ── Tasks ─────────────────────────────────────────────────────────── */

#define portTASK_FUNCTION_PROTO( vFunction, pvParameters )    void vFunction( void * pvParameters )
#define portTASK_FUNCTION( vFunction, pvParameters )          void vFunction( void * pvParameters )

/* Called by the kernel when a deleted task's TCB is freed. */
void vPortCleanUpTCB( void * pxTCB );
#define portCLEAN_UP_TCB( pxTCB )    vPortCleanUpTCB( pxTCB )

/* ── Idle ──────────────────────────────────────────────────────────── */

void vPortSuppressTicksAndSleep( TickType_t xExpectedIdleTime );
#define portSUPPRESS_TICKS_AND_SLEEP( xExpectedIdleTime )    vPortSuppressTicksAndSleep( xExpectedIdleTime )

/*
 * The idle task only calls into the port when tickless idle decides to sleep
 * for 2+ ticks.  Otherwise it spins, and on a cooperative CPU time would never
 * advance.  configCONTROL_INFINITE_LOOP() is evaluated once per iteration of
 * the idle (and timer) task loops, which gives the port a place to halt.
 */
void vPortTaskLoopIteration( void );
#ifndef configCONTROL_INFINITE_LOOP
    #define configCONTROL_INFINITE_LOOP()    ( vPortTaskLoopIteration(), 1 )
#endif

/* vPortTaskLoopIteration() needs to recognise the idle task. */
#ifndef INCLUDE_xTaskGetIdleTaskHandle
    #define INCLUDE_xTaskGetIdleTaskHandle    1
#endif

#ifdef __cplusplus
}
#endif

#endif /* PORTMACRO_H */
