#ifndef PORTMACRO_H
#define PORTMACRO_H

#include <stdint.h>
#include <stddef.h>

/* ── Simulator ABI ──────────────────────────────────────────────────── */

#include "sim_abi.h"

/* ── Data types ────────────────────────────────────────────────────── */

#define portCHAR          char
#define portFLOAT         float
#define portDOUBLE        double
#define portLONG          long
#define portSHORT         short
#define portSTACK_TYPE    uint32_t
#define portBASE_TYPE     long
#define portUBASE_TYPE    unsigned long

typedef portSTACK_TYPE StackType_t;
typedef long BaseType_t;
typedef unsigned long UBaseType_t;
typedef uint32_t TickType_t;

/** Task function signature. */
typedef void (*TaskFunction_t)(void *);

/** Used to hide the actual task handle so the application can't peek. */
typedef void *TaskHandle_t;

/* ── Critical sections ─────────────────────────────────────────────── */

#define portENTER_CRITICAL()      sim_enter_critical()
#define portEXIT_CRITICAL()       sim_exit_critical()

/* ── Yielding ───────────────────────────────────────────────────────── */

#define portYIELD()               sim_port_yield()
#define portYIELD_FROM_ISR(x)     sim_port_yield()

/* ── Task utilities ────────────────────────────────────────────────── */

#define portTASK_FUNCTION_PROTO( vFunction, pvParameters ) \
    void vFunction( void * pvParameters )
#define portTASK_FUNCTION( vFunction, pvParameters ) \
    void vFunction( void * pvParameters )

/* ── Memory ─────────────────────────────────────────────────────────── */

#define portBYTE_ALIGNMENT        8
#define portBYTE_ALIGNMENT_MASK   ( 0x0007 )

/* ── Kernel interface ───────────────────────────────────────────────── */

/* Maximum number of priorities (MVP: 8). */
#define configMAX_PRIORITIES      ( 8 )

/* MVP: use a periodic tick of 1ms = 1,000,000 ns (1 ns tick unit). */
#define configTICK_RATE_HZ        ( (TickType_t) 1000 )

/* The maximum number of task priorities. */
#define configUSE_PREEMPTION      0  /* cooperative only for MVP */

/* Idle task hook: not used in MVP. */
#define configUSE_IDLE_HOOK       0

/* Tick hook: not used in MVP. */
#define configUSE_TICK_HOOK       0

/* Minimal stack size in words. */
#define configMINIMAL_STACK_SIZE  ( (uint16_t) 128 )

/* Maximum task name length. */
#define configMAX_TASK_NAME_LEN   ( 16 )

/* Use 16-bit tick type for MVP. */
#define configUSE_16_BIT_TICKS    0

/* Queue registry not needed for MVP. */
#define configQUEUE_REGISTRY_SIZE 0

/* Timer task: not used in MVP yet. */
#define configUSE_TIMERS          0

/* ── Scheduler control ──────────────────────────────────────────────── */

BaseType_t xPortStartScheduler( void );
void vPortEndScheduler( void );

/* ── Stack initialisation ──────────────────────────────────────────── */

/**
 * In our simulator port this function does NOT create a real CPU stack
 * frame.  Instead it stores the task entry-point and parameter in the
 * stack array as metadata, and the Rust runtime creates the actual
 * coroutine when the scheduler starts.
 */
StackType_t *pxPortInitialiseStack(
    StackType_t *pxTopOfStack,
    TaskFunction_t pxCode,
    void *pvParameters
);

/* ── Tick suppression (not implemented in MVP) ─────────────────────── */

#define portSUPPRESS_TICKS_AND_SLEEP( xExpectedIdleTime )

/* ── Architecture specifics (simulated) ─────────────────────────────── */

#define portNOP()                  /* nothing */
#define portINLINE                 static inline

static inline uint32_t portDISABLE_INTERRUPTS(void) {
    sim_enter_critical();
    return 0;
}

static inline void portENABLE_INTERRUPTS(uint32_t ulPreviousState) {
    (void)ulPreviousState;
    sim_exit_critical();
}

#endif /* PORTMACRO_H */
