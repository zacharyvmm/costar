/*
 * FreeRTOSConfig.h — Simulator port configuration
 *
 * Minimal configuration for the Universal RTOS Native Simulator.
 * Most features are disabled to keep the build simple and
 * match the MVP scope.
 */

#ifndef FREERTOS_CONFIG_H
#define FREERTOS_CONFIG_H

/* ── Scheduler ─────────────────────────────────────────────────────── */

#define configUSE_PREEMPTION                    0
#define configUSE_PORT_OPTIMISED_TASK_SELECTION 0
#define configUSE_TICKLESS_IDLE                 0
#define configTICK_RATE_HZ                      1000
#define configMAX_PRIORITIES                    8
#define configMINIMAL_STACK_SIZE                128
#define configMAX_TASK_NAME_LEN                 16
#define configUSE_16_BIT_TICKS                  0
#define configIDLE_SHOULD_YIELD                 1
#define configUSE_TASK_NOTIFICATIONS            1
#define configTASK_NOTIFICATION_ARRAY_ENTRIES   1
#define configUSE_MUTEXES                       1
#define configUSE_RECURSIVE_MUTEXES             1
#define configUSE_COUNTING_SEMAPHORES           1
#define configUSE_EVENT_GROUPS                  1
#define configQUEUE_REGISTRY_SIZE               0
#define configUSE_QUEUE_SETS                    0
#define configUSE_TIME_SLICING                  0
#define configUSE_NEWLIB_REENTRANT              0
#define configENABLE_BACKWARD_COMPATIBILITY     0
#define configNUM_THREAD_LOCAL_STORAGE_POINTERS  1
#define configSTACK_DEPTH_TYPE                  uint16_t

/* ── Memory ────────────────────────────────────────────────────────── */

#define configSUPPORT_STATIC_ALLOCATION         1
#define configSUPPORT_DYNAMIC_ALLOCATION        1
#define configTOTAL_HEAP_SIZE                   ( (size_t) 32768 )
#define configAPPLICATION_ALLOCATED_HEAP        0

/* ── Hooks ─────────────────────────────────────────────────────────── */

#define configUSE_IDLE_HOOK                     0
#define configUSE_TICK_HOOK                     0
#define configCHECK_FOR_STACK_OVERFLOW          0
#define configUSE_MALLOC_FAILED_HOOK            0
#define configUSE_DAEMON_TASK_STARTUP_HOOK      0

/* ── Runtime stats ─────────────────────────────────────────────────── */

#define configGENERATE_RUN_TIME_STATS           0
#define configUSE_TRACE_FACILITY                0
#define configUSE_STATS_FORMATTING_FUNCTIONS    0

/* ── Co-routines ───────────────────────────────────────────────────── */

#define configUSE_CO_ROUTINES                   0
#define configMAX_CO_ROUTINE_PRIORITIES         1

/* ── Timers ────────────────────────────────────────────────────────── */

#define configUSE_TIMERS                        1
#define configTIMER_TASK_PRIORITY               2
#define configTIMER_QUEUE_LENGTH                10
#define configTIMER_TASK_STACK_DEPTH            256

/* ── Optional APIs ─────────────────────────────────────────────────── */

#define INCLUDE_vTaskPrioritySet                0
#define INCLUDE_uxTaskPriorityGet               0
#define INCLUDE_vTaskDelete                     1
#define INCLUDE_vTaskSuspend                    0
#define INCLUDE_vTaskDelayUntil                 1
#define INCLUDE_vTaskDelay                      1
#define INCLUDE_xTaskGetSchedulerState          0
#define INCLUDE_xTaskGetCurrentTaskHandle       1
#define INCLUDE_uxTaskGetStackHighWaterMark     0
#define INCLUDE_xTaskGetIdleTaskHandle          0
#define INCLUDE_eTaskGetState                   0
#define INCLUDE_xTaskAbortDelay                 0
#define INCLUDE_xTaskGetHandle                  0

/* ── Assert ────────────────────────────────────────────────────────── */

#define configASSERT( x )    ( ( void ) 0 )

/* ── Trace hooks ───────────────────────────────────────────────────── */

/* After FreeRTOS fully initialises a new TCB, create the corresponding
 * Rust fiber and store the handle in the TCB's simHandle field. */
#define traceTASK_CREATE( pxNewTCB )    sim_port_task_created( pxNewTCB )

/* ── Initial tick count ────────────────────────────────────────────── */

#define configINITIAL_TICK_COUNT                0

#endif /* FREERTOS_CONFIG_H */
