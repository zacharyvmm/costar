/*
 * FreeRTOS.h
 *
 * Top-level include for the FreeRTOS kernel (simulator port).
 */

#ifndef FREERTOS_H
#define FREERTOS_H

#include <stddef.h>
#include <stdint.h>

/* ── Kernel configuration ──────────────────────────────────────────── */

#if !defined(configUSE_PREEMPTION)
    #define configUSE_PREEMPTION    0
#endif

#if !defined(configUSE_16_BIT_TICKS)
    #define configUSE_16_BIT_TICKS  0
#endif

/* ── Data types ────────────────────────────────────────────────────── */

#define pdFALSE         ( ( BaseType_t ) 0 )
#define pdTRUE          ( ( BaseType_t ) 1 )

#define pdPASS          ( pdTRUE )
#define pdFAIL          ( pdFALSE )

#define pdMS_TO_TICKS( xTimeInMs ) \
    ( ( TickType_t ) ( ( ( uint64_t ) ( xTimeInMs ) * ( uint64_t ) configTICK_RATE_HZ ) / 1000U ) )

/* ── Scheduler status ──────────────────────────────────────────────── */

typedef enum
{
    taskNOT_YET_STARTED = 0,
    running = 1,
    suspended = 2,
} eTaskState;

/* ── Task handle ───────────────────────────────────────────────────── */

typedef void * TaskHandle_t;

/* ── Function prototypes ───────────────────────────────────────────── */

/* Include sub-headers after type definitions. */
#include "portmacro.h"
#include "task.h"
#include "queue.h"
#include "list.h"

#endif /* FREERTOS_H */
