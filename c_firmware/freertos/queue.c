/*
 * queue.c — Queue implementation (minimal)
 *
 * Simple ring-buffer queue for the MVP simulator port.
 * Does NOT block on send/receive (returns immediately).
 */

#include "FreeRTOS.h"
#include "queue.h"
#include "task.h"

#include <stddef.h>
#include <string.h>

/* ── Queue structure ───────────────────────────────────────────────── */

typedef struct QueueDefinition
{
    uint8_t     *pcHead;            /* Start of storage area */
    uint8_t     *pcWriteTo;         /* Next write position */
    uint8_t     *pcReadFrom;        /* Next read position */
    UBaseType_t  uxLength;          /* Max items */
    UBaseType_t  uxItemSize;        /* Size of each item */
    UBaseType_t  uxMessagesWaiting; /* Current count */
} Queue_t;

/* ── Pool of queues ────────────────────────────────────────────────── */

#define MAX_QUEUES  8

static Queue_t xQueuePool[ MAX_QUEUES ];
static uint8_t xQueueStorage[ MAX_QUEUES ][ 256 ]; /* max 256 bytes per queue */
static int queueIndex = 0;

/* ── Create ────────────────────────────────────────────────────────── */

QueueHandle_t xQueueCreate( UBaseType_t uxQueueLength, UBaseType_t uxItemSize )
{
    Queue_t *pxQueue;

    if( queueIndex >= MAX_QUEUES )
    {
        return NULL;
    }

    if( ( uxQueueLength * uxItemSize ) > 256 )
    {
        return NULL; /* too large for MVP static pool */
    }

    pxQueue = &xQueuePool[ queueIndex ];

    pxQueue->pcHead = xQueueStorage[ queueIndex ];
    pxQueue->pcWriteTo = pxQueue->pcHead;
    pxQueue->pcReadFrom = pxQueue->pcHead;
    pxQueue->uxLength = uxQueueLength;
    pxQueue->uxItemSize = uxItemSize;
    pxQueue->uxMessagesWaiting = 0;

    queueIndex++;

    return ( QueueHandle_t ) pxQueue;
}

/* ── Send ──────────────────────────────────────────────────────────── */

BaseType_t xQueueSend(
    QueueHandle_t xQueue,
    const void *pvItemToQueue,
    TickType_t xTicksToWait
)
{
    Queue_t *pxQueue = ( Queue_t * ) xQueue;

    (void) xTicksToWait; /* non-blocking in MVP */

    if( pxQueue == NULL )
    {
        return pdFALSE;
    }

    taskENTER_CRITICAL();
    {
        if( pxQueue->uxMessagesWaiting >= pxQueue->uxLength )
        {
            taskEXIT_CRITICAL();
            return pdFALSE; /* queue full */
        }

        /* Copy item to queue. */
        memcpy( pxQueue->pcWriteTo, pvItemToQueue, pxQueue->uxItemSize );

        /* Advance write pointer (ring buffer). */
        pxQueue->pcWriteTo += pxQueue->uxItemSize;
        if( pxQueue->pcWriteTo >=
            ( pxQueue->pcHead + ( pxQueue->uxLength * pxQueue->uxItemSize ) ) )
        {
            pxQueue->pcWriteTo = pxQueue->pcHead;
        }

        pxQueue->uxMessagesWaiting++;
    }
    taskEXIT_CRITICAL();

    return pdPASS;
}

/* ── Send from ISR ─────────────────────────────────────────────────── */

BaseType_t xQueueSendFromISR(
    QueueHandle_t xQueue,
    const void *pvItemToQueue,
    BaseType_t *pxHigherPriorityTaskWoken
)
{
    BaseType_t result = xQueueSend( xQueue, pvItemToQueue, 0 );

    if( pxHigherPriorityTaskWoken != NULL )
    {
        *pxHigherPriorityTaskWoken = pdFALSE;
    }

    return result;
}

/* ── Send to front ─────────────────────────────────────────────────── */

BaseType_t xQueueSendToFront(
    QueueHandle_t xQueue,
    const void *pvItemToQueue,
    TickType_t xTicksToWait
)
{
    Queue_t *pxQueue = ( Queue_t * ) xQueue;

    (void) xTicksToWait;

    if( pxQueue == NULL )
    {
        return pdFALSE;
    }

    taskENTER_CRITICAL();
    {
        if( pxQueue->uxMessagesWaiting >= pxQueue->uxLength )
        {
            taskEXIT_CRITICAL();
            return pdFALSE;
        }

        /* Move read pointer back to insert at front. */
        if( pxQueue->pcReadFrom == pxQueue->pcHead )
        {
            pxQueue->pcReadFrom =
                pxQueue->pcHead +
                ( ( pxQueue->uxLength - 1 ) * pxQueue->uxItemSize );
        }
        else
        {
            pxQueue->pcReadFrom -= pxQueue->uxItemSize;
        }

        memcpy( pxQueue->pcReadFrom, pvItemToQueue, pxQueue->uxItemSize );
        pxQueue->uxMessagesWaiting++;
    }
    taskEXIT_CRITICAL();

    return pdPASS;
}

/* ── Receive ───────────────────────────────────────────────────────── */

BaseType_t xQueueReceive(
    QueueHandle_t xQueue,
    void *pvBuffer,
    TickType_t xTicksToWait
)
{
    Queue_t *pxQueue = ( Queue_t * ) xQueue;

    (void) xTicksToWait; /* non-blocking in MVP */

    if( pxQueue == NULL )
    {
        return pdFALSE;
    }

    taskENTER_CRITICAL();
    {
        if( pxQueue->uxMessagesWaiting == 0 )
        {
            taskEXIT_CRITICAL();
            return pdFALSE; /* queue empty */
        }

        /* Copy item from queue. */
        memcpy( pvBuffer, pxQueue->pcReadFrom, pxQueue->uxItemSize );

        /* Advance read pointer (ring buffer). */
        pxQueue->pcReadFrom += pxQueue->uxItemSize;
        if( pxQueue->pcReadFrom >=
            ( pxQueue->pcHead + ( pxQueue->uxLength * pxQueue->uxItemSize ) ) )
        {
            pxQueue->pcReadFrom = pxQueue->pcHead;
        }

        pxQueue->uxMessagesWaiting--;
    }
    taskEXIT_CRITICAL();

    return pdPASS;
}

/* ── Receive from ISR ──────────────────────────────────────────────── */

BaseType_t xQueueReceiveFromISR(
    QueueHandle_t xQueue,
    void *pvBuffer,
    BaseType_t *pxHigherPriorityTaskWoken
)
{
    BaseType_t result = xQueueReceive( xQueue, pvBuffer, 0 );

    if( pxHigherPriorityTaskWoken != NULL )
    {
        *pxHigherPriorityTaskWoken = pdFALSE;
    }

    return result;
}

/* ── Peek ──────────────────────────────────────────────────────────── */

BaseType_t xQueuePeek(
    QueueHandle_t xQueue,
    void *pvBuffer,
    TickType_t xTicksToWait
)
{
    Queue_t *pxQueue = ( Queue_t * ) xQueue;

    (void) xTicksToWait;

    if( pxQueue == NULL || pxQueue->uxMessagesWaiting == 0 )
    {
        return pdFALSE;
    }

    taskENTER_CRITICAL();
    {
        memcpy( pvBuffer, pxQueue->pcReadFrom, pxQueue->uxItemSize );
    }
    taskEXIT_CRITICAL();

    return pdPASS;
}

/* ── Query ─────────────────────────────────────────────────────────── */

UBaseType_t uxQueueMessagesWaiting( QueueHandle_t xQueue )
{
    Queue_t *pxQueue = ( Queue_t * ) xQueue;
    if( pxQueue == NULL ) return 0;
    return pxQueue->uxMessagesWaiting;
}

UBaseType_t uxQueueSpacesAvailable( QueueHandle_t xQueue )
{
    Queue_t *pxQueue = ( Queue_t * ) xQueue;
    if( pxQueue == NULL ) return 0;
    return pxQueue->uxLength - pxQueue->uxMessagesWaiting;
}

void vQueueDelete( QueueHandle_t xQueue )
{
    /* Static pool — just reset. */
    Queue_t *pxQueue = ( Queue_t * ) xQueue;
    if( pxQueue != NULL )
    {
        pxQueue->uxMessagesWaiting = 0;
        pxQueue->pcWriteTo = pxQueue->pcHead;
        pxQueue->pcReadFrom = pxQueue->pcHead;
    }
}

BaseType_t xQueueReset( QueueHandle_t xQueue )
{
    Queue_t *pxQueue = ( Queue_t * ) xQueue;
    if( pxQueue == NULL ) return pdFALSE;
    pxQueue->uxMessagesWaiting = 0;
    pxQueue->pcWriteTo = pxQueue->pcHead;
    pxQueue->pcReadFrom = pxQueue->pcHead;
    return pdPASS;
}
