/*
 * queue.h — Queue API (minimal)
 */

#ifndef FREERTOS_QUEUE_H
#define FREERTOS_QUEUE_H

#include "portmacro.h"

/* ── Queue handle ──────────────────────────────────────────────────── */

typedef void * QueueHandle_t;

/* ── Queue creation ────────────────────────────────────────────────── */

/**
 * Create a queue.
 *
 * @param uxQueueLength   Maximum number of items.
 * @param uxItemSize      Size of each item in bytes.
 * @return Handle to the queue, or NULL on failure.
 */
QueueHandle_t xQueueCreate( UBaseType_t uxQueueLength, UBaseType_t uxItemSize );

/* ── Send ──────────────────────────────────────────────────────────── */

/**
 * Post an item to the back of a queue.
 * Does NOT block in the MVP (returns immediately).
 */
BaseType_t xQueueSend(
    QueueHandle_t xQueue,
    const void *pvItemToQueue,
    TickType_t xTicksToWait
);

/** Post to the back of a queue (ISR version). */
BaseType_t xQueueSendFromISR(
    QueueHandle_t xQueue,
    const void *pvItemToQueue,
    BaseType_t *pxHigherPriorityTaskWoken
);

/** Post to the front of a queue. */
BaseType_t xQueueSendToFront(
    QueueHandle_t xQueue,
    const void *pvItemToQueue,
    TickType_t xTicksToWait
);

/** Post to the back of a queue (simpler API name). */
#define xQueueSendToBack(xQueue, pvItemToQueue, xTicksToWait) \
    xQueueSend((xQueue), (pvItemToQueue), (xTicksToWait))

/* ── Receive ───────────────────────────────────────────────────────── */

/**
 * Receive an item from a queue.
 * Does NOT block in the MVP (returns immediately).
 */
BaseType_t xQueueReceive(
    QueueHandle_t xQueue,
    void *pvBuffer,
    TickType_t xTicksToWait
);

/** Receive from a queue (ISR version). */
BaseType_t xQueueReceiveFromISR(
    QueueHandle_t xQueue,
    void *pvBuffer,
    BaseType_t *pxHigherPriorityTaskWoken
);

/* ── Peek ──────────────────────────────────────────────────────────── */

/** Peek at an item without removing it. */
BaseType_t xQueuePeek(
    QueueHandle_t xQueue,
    void *pvBuffer,
    TickType_t xTicksToWait
);

/* ── Query ─────────────────────────────────────────────────────────── */

/** Number of messages in the queue. */
UBaseType_t uxQueueMessagesWaiting( QueueHandle_t xQueue );

/** Number of free spaces in the queue. */
UBaseType_t uxQueueSpacesAvailable( QueueHandle_t xQueue );

/** Delete a queue. */
void vQueueDelete( QueueHandle_t xQueue );

/** Reset a queue. */
BaseType_t xQueueReset( QueueHandle_t xQueue );

#endif /* FREERTOS_QUEUE_H */
