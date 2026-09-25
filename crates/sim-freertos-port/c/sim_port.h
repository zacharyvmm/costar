/*
 * sim_port.h — private definitions shared by port.c and the patched tasks.c.
 */

#ifndef SIM_PORT_H
#define SIM_PORT_H

#include <stdint.h>

#define SIM_PORT_FRAME_MAGIC 0x53494D46u /* "SIMF" */

/*
 * pxPortInitialiseStack() stores the task's entry point and parameter in
 * this frame at the top of the FreeRTOS-allocated stack; traceTASK_CREATE
 * reads it back to create the task's fiber.  Pointers are stored at full
 * host width (StackType_t is only 32 bits).
 *
 * The task never executes on this stack: its fiber has its own host stack.
 */
typedef struct SimPortFrame
{
    uint32_t magic;
    uint32_t reserved;
    void ( *code )( void * );
    void *param;
} SimPortFrame;

#endif /* SIM_PORT_H */
