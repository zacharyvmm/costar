/*
 * A FreeRTOS heap for sanitizer builds (costar_add_freertos_image(... HEAP
 * heap_costar), the default when COSTAR_SANITIZE includes address).
 *
 * Like heap_3, each pvPortMalloc() is a host malloc() of its own, so
 * AddressSanitizer catches overruns, use after free and double frees in
 * firmware.  Unlike heap_3, it keeps a list of the live blocks and frees them
 * when the image is unloaded: a machine that powers off loses its RAM, which
 * is not a leak.
 *
 * It has heap_4's API (pvPortCalloc, xPortGetFreeHeapSize, vPortGetHeapStats,
 * ...) and, if configTOTAL_HEAP_SIZE is defined, its limit: an allocation
 * that would take more than that in all fails.  Sizes count the bytes asked
 * for, without heap_4's per-block overhead, and the free space is one block.
 */
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "FreeRTOS.h"
#include "task.h"

#if defined( __SANITIZE_ADDRESS__ )
    #define COSTAR_HEAP_ASAN    1
#elif defined( __has_feature )
    #if __has_feature( address_sanitizer )
        #define COSTAR_HEAP_ASAN    1
    #endif
#endif

#ifdef COSTAR_HEAP_ASAN
    #include <sanitizer/asan_interface.h>
#else
    #define ASAN_POISON_MEMORY_REGION( addr, size )      ( ( void ) ( addr ), ( void ) ( size ) )
    #define ASAN_UNPOISON_MEMORY_REGION( addr, size )    ( ( void ) ( addr ), ( void ) ( size ) )
#endif

#ifdef configTOTAL_HEAP_SIZE
    #define heapTOTAL    ( ( size_t ) configTOTAL_HEAP_SIZE )
#else
    #define heapTOTAL    SIZE_MAX
#endif

/* Precedes each block; its size keeps the block aligned as malloc() does.
 * Poisoned while the heap isn't looking at it, so firmware writing before a
 * block it allocated (an underrun) is caught.
 */
typedef union Block
{
    struct
    {
        union Block * pxPrevious;
        union Block * pxNext;
        size_t xSize;
    } xHeader;
    max_align_t xAlignment;
} Block_t;

static Block_t xLive = { { &xLive, &xLive, 0 } };
static size_t xUsed = 0;
static size_t xMostUsed = 0;
static size_t xAllocations = 0;
static size_t xFrees = 0;

static void prvOpen( Block_t * pxBlock )
{
    if( pxBlock != &xLive )
    {
        ASAN_UNPOISON_MEMORY_REGION( pxBlock, sizeof( Block_t ) );
    }
}

static void prvClose( Block_t * pxBlock )
{
    if( pxBlock != &xLive )
    {
        ASAN_POISON_MEMORY_REGION( pxBlock, sizeof( Block_t ) );
    }
}

void * pvPortMalloc( size_t xWantedSize )
{
    Block_t * pxBlock = NULL;

    vTaskSuspendAll();
    {
        if( ( xWantedSize <= SIZE_MAX - sizeof( Block_t ) ) && ( xWantedSize <= heapTOTAL - xUsed ) )
        {
            pxBlock = malloc( sizeof( Block_t ) + xWantedSize );
        }

        if( pxBlock != NULL )
        {
            Block_t * pxNext = xLive.xHeader.pxNext;

            pxBlock->xHeader.pxPrevious = &xLive;
            pxBlock->xHeader.pxNext = pxNext;
            pxBlock->xHeader.xSize = xWantedSize;
            prvOpen( pxNext );
            pxNext->xHeader.pxPrevious = pxBlock;
            prvClose( pxNext );
            xLive.xHeader.pxNext = pxBlock;
            prvClose( pxBlock );

            xUsed += xWantedSize;
            xMostUsed = xUsed > xMostUsed ? xUsed : xMostUsed;
            xAllocations++;
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

void * pvPortCalloc( size_t xNum,
                     size_t xSize )
{
    void * pv = NULL;

    if( ( xSize == 0 ) || ( xNum <= SIZE_MAX / xSize ) )
    {
        pv = pvPortMalloc( xNum * xSize );

        if( pv != NULL )
        {
            memset( pv, 0, xNum * xSize );
        }
    }

    return pv;
}

/* Unlink and free a block; returns its size.  With the scheduler suspended. */
static size_t prvRelease( Block_t * pxBlock )
{
    prvOpen( pxBlock );

    Block_t * pxPrevious = pxBlock->xHeader.pxPrevious;
    Block_t * pxNext = pxBlock->xHeader.pxNext;
    const size_t xSize = pxBlock->xHeader.xSize;

    prvOpen( pxPrevious );
    pxPrevious->xHeader.pxNext = pxNext;
    prvClose( pxPrevious );
    prvOpen( pxNext );
    pxNext->xHeader.pxPrevious = pxPrevious;
    prvClose( pxNext );
    free( pxBlock );
    return xSize;
}

void vPortFree( void * pv )
{
    if( pv != NULL )
    {
        vTaskSuspendAll();
        {
            const size_t xSize = prvRelease( ( Block_t * ) pv - 1 );

            xUsed -= xSize;
            xFrees++;
            traceFREE( pv, xSize );
        }
        ( void ) xTaskResumeAll();
    }
}

size_t xPortGetFreeHeapSize( void )
{
    return heapTOTAL - xUsed;
}

size_t xPortGetMinimumEverFreeHeapSize( void )
{
    return heapTOTAL - xMostUsed;
}

void xPortResetHeapMinimumEverFreeHeapSize( void )
{
    xMostUsed = xUsed;
}

void vPortGetHeapStats( HeapStats_t * pxHeapStats )
{
    vTaskSuspendAll();
    {
        const size_t xFree = heapTOTAL - xUsed;

        pxHeapStats->xAvailableHeapSpaceInBytes = xFree;
        pxHeapStats->xSizeOfLargestFreeBlockInBytes = xFree;
        pxHeapStats->xSizeOfSmallestFreeBlockInBytes = xFree;
        pxHeapStats->xNumberOfFreeBlocks = xFree > 0 ? 1 : 0;
        pxHeapStats->xMinimumEverFreeBytesRemaining = heapTOTAL - xMostUsed;
        pxHeapStats->xNumberOfSuccessfulAllocations = xAllocations;
        pxHeapStats->xNumberOfSuccessfulFrees = xFrees;
    }
    ( void ) xTaskResumeAll();
}

void vPortHeapResetState( void )
{
    /* Nothing to reset: blocks still allocated stay valid. */
}

/* The image is being unloaded: its RAM goes with it. */
__attribute__( ( destructor ) ) static void prvFreeAll( void )
{
    while( xLive.xHeader.pxNext != &xLive )
    {
        ( void ) prvRelease( xLive.xHeader.pxNext );
    }
}
