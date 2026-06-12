/*
 * list.c — Doubly-linked list implementation
 */

#include "list.h"
#include <stddef.h>

void vListInitialise( List_t *pxList )
{
    pxList->pxIndex = ( ListItem_t * ) &( pxList->xListEnd );
    pxList->xListEnd.xItemValue = ( TickType_t ) 0xffffffffUL;
    pxList->xListEnd.pxNext = ( ListItem_t * ) &( pxList->xListEnd );
    pxList->xListEnd.pxPrevious = ( ListItem_t * ) &( pxList->xListEnd );
    pxList->uxNumberOfItems = ( UBaseType_t ) 0U;
}

void vListInitialiseItem( ListItem_t *pxItem )
{
    pxItem->pvContainer = NULL;
}

void vListInsertEnd( List_t *pxList, ListItem_t *pxNewListItem )
{
    ListItem_t * const pxIndex = pxList->pxIndex;

    pxNewListItem->pxNext = pxIndex;
    pxNewListItem->pxPrevious = pxIndex->pxPrevious;
    pxIndex->pxPrevious->pxNext = pxNewListItem;
    pxIndex->pxPrevious = pxNewListItem;

    pxNewListItem->pvContainer = ( void * ) pxList;

    ( pxList->uxNumberOfItems )++;
}

void vListInsert( List_t *pxList, ListItem_t *pxNewListItem )
{
    ListItem_t *pxIterator;
    TickType_t xValueOfInsertion = pxNewListItem->xItemValue;

    /* Find the insertion point (ordered by xItemValue ascending). */
    if( xValueOfInsertion == ( TickType_t ) 0xffffffffUL )
    {
        pxIterator = pxList->xListEnd.pxPrevious;
    }
    else
    {
        for( pxIterator = ( ListItem_t * ) &( pxList->xListEnd );
             pxIterator->pxNext->xItemValue <= xValueOfInsertion;
             pxIterator = pxIterator->pxNext )
        {
            /* Empty loop — iteration only. */
        }
    }

    pxNewListItem->pxNext = pxIterator->pxNext;
    pxNewListItem->pxNext->pxPrevious = pxNewListItem;
    pxNewListItem->pxPrevious = pxIterator;
    pxIterator->pxNext = pxNewListItem;

    pxNewListItem->pvContainer = ( void * ) pxList;

    ( pxList->uxNumberOfItems )++;
}

UBaseType_t uxListRemove( ListItem_t *pxItemToRemove )
{
    List_t * const pxList = ( List_t * ) pxItemToRemove->pvContainer;

    pxItemToRemove->pxNext->pxPrevious = pxItemToRemove->pxPrevious;
    pxItemToRemove->pxPrevious->pxNext = pxItemToRemove->pxNext;

    /* Update the index if it pointed to the removed item. */
    if( pxList->pxIndex == pxItemToRemove )
    {
        pxList->pxIndex = pxItemToRemove->pxPrevious;
    }

    pxItemToRemove->pvContainer = NULL;
    ( pxList->uxNumberOfItems )--;

    return pxList->uxNumberOfItems;
}
