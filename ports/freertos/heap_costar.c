/*
 * A FreeRTOS heap for sanitizer builds (costar_add_freertos_image(... HEAP
 * heap_costar), the default when COSTAR_SANITIZE includes address).
 *
 * Like heap_3, each pvPortMalloc() is a host malloc() of its own, so
 * AddressSanitizer catches overruns, use after free and double frees in
 * firmware.  Unlike heap_3, it keeps a list of the live blocks and frees them
 * when the image is unloaded: a machine that powers off loses its RAM, which
 * is not a leak.
 */
#include <stddef.h>
#include <stdlib.h>

#include "FreeRTOS.h"
#include "task.h"

/* Precedes each block; its size keeps the block aligned as malloc() does. */
typedef union Block
{
    struct
    {
        union Block * pxPrevious;
        union Block * pxNext;
    } xLinks;
    max_align_t xAlignment;
} Block_t;

static Block_t xLive = { { &xLive, &xLive } };

void * pvPortMalloc( size_t xWantedSize )
{
    Block_t * pxBlock = NULL;

    vTaskSuspendAll();
    {
        if( xWantedSize <= SIZE_MAX - sizeof( Block_t ) )
        {
            pxBlock = malloc( sizeof( Block_t ) + xWantedSize );
        }

        if( pxBlock != NULL )
        {
            pxBlock->xLinks.pxPrevious = &xLive;
            pxBlock->xLinks.pxNext = xLive.xLinks.pxNext;
            xLive.xLinks.pxNext->xLinks.pxPrevious = pxBlock;
            xLive.xLinks.pxNext = pxBlock;
        }

        traceMALLOC( pxBlock != NULL ? pxBlock + 1 : NULL, xWantedSize );
    }
    ( void ) xTaskResumeAll();

    #if ( configUSE_MALLOC_FAILED_HOOK == 1 )
    {
        if( pxBlock == NULL )
        {
            vApplicationMallocFailedHook();
        }
    }
    #endif

    return pxBlock != NULL ? pxBlock + 1 : NULL;
}

static void prvRelease( Block_t * pxBlock )
{
    pxBlock->xLinks.pxPrevious->xLinks.pxNext = pxBlock->xLinks.pxNext;
    pxBlock->xLinks.pxNext->xLinks.pxPrevious = pxBlock->xLinks.pxPrevious;
    free( pxBlock );
}

void vPortFree( void * pv )
{
    if( pv != NULL )
    {
        vTaskSuspendAll();
        {
            prvRelease( ( Block_t * ) pv - 1 );
            traceFREE( pv, 0 );
        }
        ( void ) xTaskResumeAll();
    }
}

void vPortHeapResetState( void )
{
    /* Nothing to reset: blocks still allocated stay valid. */
}

/* The image is being unloaded: its RAM goes with it. */
__attribute__( ( destructor ) ) static void prvFreeAll( void )
{
    while( xLive.xLinks.pxNext != &xLive )
    {
        prvRelease( xLive.xLinks.pxNext );
    }
}
