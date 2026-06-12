/*
 * list.h — Doubly-linked list (used by FreeRTOS scheduler)
 *
 * Minimal implementation for the MVP simulator port.
 */

#ifndef FREERTOS_LIST_H
#define FREERTOS_LIST_H

#include "portmacro.h"

/* ── List item ─────────────────────────────────────────────────────── */

struct xLIST_ITEM
{
    TickType_t xItemValue;          /* Value used for ordering */
    struct xLIST_ITEM *pxNext;      /* Next item in list */
    struct xLIST_ITEM *pxPrevious;  /* Previous item in list */
    void *pvOwner;                  /* Owning object (TCB, etc.) */
    void *pvContainer;              /* List this item is in */
};
typedef struct xLIST_ITEM ListItem_t;

/* ── Mini list item (end marker) ───────────────────────────────────── */

struct xMINI_LIST_ITEM
{
    TickType_t xItemValue;
    struct xLIST_ITEM *pxNext;
    struct xLIST_ITEM *pxPrevious;
};
typedef struct xMINI_LIST_ITEM MiniListItem_t;

/* ── List ──────────────────────────────────────────────────────────── */

typedef struct xLIST
{
    UBaseType_t uxNumberOfItems;
    ListItem_t *pxIndex;            /* Used for round-robin traversal */
    MiniListItem_t xListEnd;        /* Sentinel */
} List_t;

/* ── Initialisation ────────────────────────────────────────────────── */

void vListInitialise( List_t *pxList );
void vListInitialiseItem( ListItem_t *pxItem );

/* ── Insert ────────────────────────────────────────────────────────── */

/** Insert at the end. */
void vListInsertEnd( List_t *pxList, ListItem_t *pxNewListItem );

/** Insert ordered by xItemValue (ascending). */
void vListInsert( List_t *pxList, ListItem_t *pxNewListItem );

/* ── Remove ────────────────────────────────────────────────────────── */

/** Remove an item and return the count after removal. */
UBaseType_t uxListRemove( ListItem_t *pxItemToRemove );

/* ── Macros ────────────────────────────────────────────────────────── */

#define listSET_LIST_ITEM_OWNER( pxListItem, pxOwner ) \
    ( ( pxListItem )->pvOwner = ( void * ) ( pxOwner ) )

#define listGET_LIST_ITEM_OWNER( pxListItem ) \
    ( ( pxListItem )->pvOwner )

#define listSET_LIST_ITEM_VALUE( pxListItem, xValue ) \
    ( ( pxListItem )->xItemValue = ( xValue ) )

#define listGET_LIST_ITEM_VALUE( pxListItem ) \
    ( ( pxListItem )->xItemValue )

#define listGET_ITEM_VALUE_OF_HEAD_ENTRY( pxList ) \
    ( ( ( pxList )->xListEnd.pxNext )->xItemValue )

#define listLIST_IS_EMPTY( pxList ) \
    ( ( ( pxList )->uxNumberOfItems == ( UBaseType_t ) 0 ) ? pdTRUE : pdFALSE )

#define listCURRENT_LIST_LENGTH( pxList ) \
    ( ( pxList )->uxNumberOfItems )

#define listGET_OWNER_OF_NEXT_ENTRY( pxTCB, pxList )                   \
{                                                                      \
    List_t * const pxConstList = ( pxList );                           \
    /* Increment the index */                                          \
    ( pxConstList )->pxIndex = ( pxConstList )->pxIndex->pxNext;       \
    if( ( void * ) ( pxConstList )->pxIndex ==                         \
        ( void * ) &( ( pxConstList )->xListEnd ) )                    \
    {                                                                  \
        ( pxConstList )->pxIndex =                                     \
            ( pxConstList )->pxIndex->pxNext;                          \
    }                                                                  \
    ( pxTCB ) = ( pxConstList )->pxIndex->pvOwner;                     \
}

#define listGET_OWNER_OF_HEAD_ENTRY( pxList ) \
    ( ( ( pxList )->xListEnd.pxNext )->pvOwner )

#endif /* FREERTOS_LIST_H */
