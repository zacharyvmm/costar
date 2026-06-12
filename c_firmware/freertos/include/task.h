/*
 * task.h — Task API
 */

#ifndef FREERTOS_TASK_H
#define FREERTOS_TASK_H

#include "portmacro.h"

/* ── Task function type ────────────────────────────────────────────── */

#ifndef TaskFunction_t
typedef void (*TaskFunction_t)(void *);
#endif

/* ── Task creation ─────────────────────────────────────────────────── */

/**
 * Create a new task and add it to the ready list.
 *
 * In the simulator port, this calls sim_create_task() via the Rust FFI.
 */
BaseType_t xTaskCreate(
    TaskFunction_t pxTaskCode,
    const char * const pcName,
    uint16_t usStackDepth,
    void *pvParameters,
    UBaseType_t uxPriority,
    TaskHandle_t *pxCreatedTask
);

/* ── Task control ──────────────────────────────────────────────────── */

/** Delay a task for a specified number of ticks. */
void vTaskDelay( TickType_t xTicksToDelay );

/** Delay until a specified absolute tick time. */
void vTaskDelayUntil( TickType_t *pxPreviousWakeTime, TickType_t xTimeIncrement );

/** Yield to another task of equal or higher priority. */
void taskYIELD(void);

/** Suspend the scheduler (disable context switching). */
void vTaskSuspendAll(void);

/** Resume the scheduler (re-enable context switching). */
BaseType_t xTaskResumeAll(void);

/** Delete a task. */
void vTaskDelete( TaskHandle_t xTaskToDelete );

/** Get the current task handle. */
TaskHandle_t xTaskGetCurrentTaskHandle(void);

/** Get the tick count. */
TickType_t xTaskGetTickCount(void);

/* ── Critical sections ─────────────────────────────────────────────── */

/**
 * Enter a critical section.
 * Returns a value to pass to taskEXIT_CRITICAL().
 */
#define taskENTER_CRITICAL()          portENTER_CRITICAL()

/** Exit a critical section. */
#define taskEXIT_CRITICAL()           portEXIT_CRITICAL()

/* ── Scheduler ─────────────────────────────────────────────────────── */

/** Start the scheduler.  Never returns. */
void vTaskStartScheduler(void);

/** Initialize the scheduler task lists (must be called before creating tasks). */
void prvInitialiseTaskLists(void);

/** End the scheduler (only in simulator). */
void vTaskEndScheduler(void);

/* ── Startup hook ──────────────────────────────────────────────────── */

/**
 * The application must provide this function.  It is called by the
 * simulator scheduler after initialization.
 */
void vApplicationStartupHook(void);

#endif /* FREERTOS_TASK_H */
