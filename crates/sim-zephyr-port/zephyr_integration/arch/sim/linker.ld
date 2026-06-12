/*
 * linker.ld — Flat linker script for the sim architecture
 *
 * Place at: include/arch/sim/linker.ld
 *
 * The simulator process runs in the host address space with no
 * memory constraints.  This trivial linker script places all
 * sections contiguously.
 */

SECTIONS
{
    . = 0;

    .text : {
        *(.text*)
    }

    .rodata : {
        *(.rodata*)
    }

    .data : {
        *(.data*)
    }

    .bss : {
        *(.bss*)
        *(COMMON)
    }

    /* Stack guard for the host process (optional).
     * Zephyr threads use corosensei-allocated stacks; this
     * section is for the host main thread only. */
    .stack (NOLOAD) : {
        . = ALIGN(16);
        *(.stack*)
    }
}
