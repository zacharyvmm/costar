/*
 * portmacro.h — Simulator port macros
 *
 * Defines the hardware abstraction layer for the Universal RTOS Native
 * Simulator.  All "hardware" operations (yield, critical sections,
 * interrupt masking) are delegated to the Rust runtime via sim_abi.h.
 */

#ifndef PORTMACRO_H
#define PORTMACRO_H

#include <stdint.h>
#include <stddef.h>

#include "sim_abi.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ── Data types ────────────────────────────────────────────────────── */

#define portCHAR          char
#define portFLOAT         float
#define portDOUBLE        double
#define portLONG          long
#define portSHORT         short
#define portSTACK_TYPE    uint32_t
#define portBASE_TYPE     long
#define portUBASE_TYPE    unsigned long
#define portPOINTER_SIZE_TYPE  uintptr_t

typedef portSTACK_TYPE  StackType_t;
typedef long            BaseType_t;
typedef unsigned long   UBaseType_t;
typedef uint32_t        TickType_t;
typedef void (* TaskFunction_t)( void * );

/* ── Architecture ──────────────────────────────────────────────────── */

#define portMAX_DELAY               ( ( TickType_t ) 0xFFFFFFFFUL )
#define portSTACK_GROWTH            (-1)
#define portTICK_PERIOD_MS          ((TickType_t) 1)
#define portBYTE_ALIGNMENT          8
#define portNOP()

/* ── Critical sections ─────────────────────────────────────────────── */

/* Interrupt masking and critical-section nesting are tracked by the engine.
 * Virtual IRQs and deferred yields are delivered once both are released. */
#define portDISABLE_INTERRUPTS()            sim_disable_interrupts()
#define portENABLE_INTERRUPTS()             sim_enable_interrupts()

#define portENTER_CRITICAL()                vPortEnterCritical()
#define portEXIT_CRITICAL()                 vPortExitCritical()

/* Virtual ISRs run on the engine's own stack between task slices, never
 * nested inside a task, so there is no mask to save. */
#define portSET_INTERRUPT_MASK_FROM_ISR()   0
#define portCLEAR_INTERRUPT_MASK_FROM_ISR(x) ((void)(x))

void vPortEnterCritical( void );
void vPortExitCritical( void );

/* ── Yielding ──────────────────────────────────────────────────────── */

/* A yield asks the engine to run vTaskSwitchContext() and resume the task
 * FreeRTOS selects (the PendSV equivalent). */
#define portYIELD()                 vPortYield()
#define portYIELD_WITHIN_API()      vPortYield()
#define portYIELD_FROM_ISR(x)       do { if( ( x ) != 0 ) { sim_port_yield_from_isr(); } } while( 0 )
#define portEND_SWITCHING_ISR(x)    portYIELD_FROM_ISR( x )

void vPortYield( void );

/* ── Task utilities ────────────────────────────────────────────────── */

#define portTASK_FUNCTION_PROTO( vFunction, pvParameters ) \
    void vFunction( void * pvParameters )
#define portTASK_FUNCTION( vFunction, pvParameters ) \
    void vFunction( void * pvParameters )

/* ── Scheduler ─────────────────────────────────────────────────────── */

BaseType_t xPortStartScheduler( void );
void vPortEndScheduler( void );

/* ── Stack initialisation ──────────────────────────────────────────── */

StackType_t *pxPortInitialiseStack(
    StackType_t *pxTopOfStack,
    TaskFunction_t pxCode,
    void *pvParameters
);

/* ── Tick suppression ──────────────────────────────────────────────── */

#define portSUPPRESS_TICKS_AND_SLEEP( xExpectedIdleTime )

/* ── Task creation hook ────────────────────────────────────────────── */

#ifdef __cplusplus
}
#endif

#endif /* PORTMACRO_H */
