/*
 * busy_poll.c — preemption points for CPU-bound firmware.
 *
 * costar's CPU is infinitely fast: time only moves while firmware waits.  A
 * busy loop (`while (!ready) {}`) would therefore stop the whole world.
 * Application code compiled with -fsanitize-coverage=trace-pc calls
 * __sanitizer_cov_trace_pc() on every basic block.  Every COSTAR_POLL_BLOCKS
 * blocks, this charges their time and lets interrupts in, through
 * costar_cpu_poll(), so ticks and devices can preempt the loop.
 *
 * Linked into images built with costar_add_freertos_image(... INSTRUMENT).
 * This file itself must not be instrumented.
 */
#include "costar.h"

#ifndef COSTAR_POLL_BLOCKS
    #define COSTAR_POLL_BLOCKS    1000
#endif

#ifndef COSTAR_BLOCK_NS
    #define COSTAR_BLOCK_NS       10
#endif

static unsigned blocks;

void __sanitizer_cov_trace_pc( void )
{
    if( ++blocks < COSTAR_POLL_BLOCKS )
    {
        return;
    }

    blocks = 0;
    costar_cpu_poll( ( costar_time ) COSTAR_POLL_BLOCKS * COSTAR_BLOCK_NS );
}
