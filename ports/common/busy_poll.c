/*
 * busy_poll.c — CPU time for instrumented firmware.
 *
 * costar's CPU is infinitely fast: time only moves while firmware waits.  A
 * busy loop (`while (!ready) {}`) would therefore stop the whole world, and
 * work never takes time, so a deadline can't be missed.  Code compiled with
 * -fsanitize-coverage=trace-pc calls __sanitizer_cov_trace_pc() on every
 * basic block.  Every COSTAR_POLL_BLOCKS blocks, this charges their time at
 * the machine's speed (its time per block) and lets interrupts in, through
 * costar_cpu_work(), so ticks and devices can preempt the code.
 *
 * Linked into images built with INSTRUMENT or INSTRUMENT_KERNEL (see
 * cmake/CostarFirmware.cmake).  This file itself must not be instrumented.
 */
#include "costar.h"

#ifndef COSTAR_POLL_BLOCKS
    #define COSTAR_POLL_BLOCKS    1000
#endif

/* Defined by a port that keeps its own copy of the time (the Zephyr
 * runner's), to refresh it after time has passed. */
void costar_busy_poll_done( void ) __attribute__( ( weak ) );

static unsigned blocks;

void __sanitizer_cov_trace_pc( void )
{
    if( ++blocks < COSTAR_POLL_BLOCKS )
    {
        return;
    }

    blocks = 0;
    costar_cpu_work( COSTAR_POLL_BLOCKS );

    if( costar_busy_poll_done != 0 )
    {
        costar_busy_poll_done();
    }
}
