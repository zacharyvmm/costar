#include "FreeRTOS.h"
#include "task.h"
#include "queue.h"
#include <stdint.h>
#include <string.h>
#include "sim_abi.h"

/* ── CAN controller ID ─────────────────────────────────────────────── */

#define CAN_BUS 0

/* ── Task A: CAN sender (sends various CAN frames) ──────────────────── */

static void vTaskA( void *pvParameters )
{
    (void) pvParameters;

    /* Enable loopback mode so sent frames appear in RX queue */
    sim_can_set_loopback( CAN_BUS, 1 );

    /* 1. Send a standard data frame with 3 bytes */
    {
        uint8_t data[] = { 0xAA, 0xBB, 0xCC };
        uint32_t rc = sim_can_send( CAN_BUS, 0x100, data, 3, 0, 0 );
        sim_trace_u32( "can_tx_send", rc );
    }

    /* 2. Send an extended (29-bit) data frame with 4 bytes */
    {
        uint8_t data[] = { 0x01, 0x02, 0x03, 0x04 };
        uint32_t rc = sim_can_send( CAN_BUS, 0x1ABCDEF, data, 4, 1, 0 );
        sim_trace_u32( "can_tx_ext", rc );
    }

    /* 3. Send a remote frame (RTR — no data payload) */
    {
        uint32_t rc = sim_can_send( CAN_BUS, 0x7FF, NULL, 0, 0, 1 );
        sim_trace_u32( "can_tx_rtr", rc );
    }

    /* 4. Inject a frame from an external node (not loopback) */
    {
        uint8_t data[] = { 0x42, 0x99 };
        sim_can_inject_rx( CAN_BUS, 0x300, data, 2, 0 );
        sim_trace_u32( "can_injected", 1 );
    }

    /* 5. Check error state (should be Error Active = 0) */
    {
        uint32_t err = sim_can_get_error( CAN_BUS );
        sim_trace_u32( "can_error", err );
    }

    sim_trace_u32( "taskA_done", 1 );
}

/* ── Task B: CAN receiver (reads frames from RX queue in loopback) ──── */

static void vTaskB( void *pvParameters )
{
    (void) pvParameters;

    /* 1. Receive the first frame (std data: ID 0x100, 3 bytes) */
    {
        uint8_t buf[8];
        uint32_t can_id = 0, is_ext = 0, is_remote = 0;
        uint32_t dlc = sim_can_recv( CAN_BUS, buf, 8, &can_id, &is_ext, &is_remote );
        sim_trace_u32( "can_rx_dlc", dlc );
        if ( dlc > 0 )
        {
            sim_trace_u32( "can_rx_id", can_id );
            sim_trace_u32( "can_rx_ext", is_ext );
            sim_trace_u32( "can_rx_rem", is_remote );
            sim_trace_u32( "can_rx_b0", buf[0] );
            sim_trace_u32( "can_rx_b1", buf[1] );
        }
    }

    /* 2. Receive the second frame (ext data: ID 0x1ABCDEF, 4 bytes) */
    {
        uint8_t buf[8];
        uint32_t can_id = 0, is_ext = 0, is_remote = 0;
        uint32_t dlc = sim_can_recv( CAN_BUS, buf, 8, &can_id, &is_ext, &is_remote );
        sim_trace_u32( "can_rx2_dlc", dlc );
        if ( dlc > 0 )
        {
            sim_trace_u32( "can_rx2_id", can_id );
            sim_trace_u32( "can_rx2_ext", is_ext );
            sim_trace_u32( "can_rx2_b0", buf[0] );
            sim_trace_u32( "can_rx2_b3", buf[3] );
        }
    }

    /* 3. Receive the third frame (RTR: ID 0x7FF, no data) */
    {
        uint8_t buf[8];
        uint32_t can_id = 0, is_ext = 0, is_remote = 0;
        uint32_t dlc = sim_can_recv( CAN_BUS, buf, 8, &can_id, &is_ext, &is_remote );
        sim_trace_u32( "can_rx_rtr", dlc );
        if ( is_remote )
        {
            sim_trace_u32( "can_rx_rtr_id", can_id );
        }
    }

    /* 4. Receive the injected frame (ID 0x300, 2 bytes) */
    {
        uint8_t buf[8];
        uint32_t can_id = 0, is_ext = 0, is_remote = 0;
        uint32_t dlc = sim_can_recv( CAN_BUS, buf, 8, &can_id, &is_ext, &is_remote );
        sim_trace_u32( "can_rx_inj", dlc );
        if ( dlc > 0 )
        {
            sim_trace_u32( "can_rx_inj_id", can_id );
            sim_trace_u32( "can_rx_inj_b0", buf[0] );
        }
    }

    /* 5. Try to receive when queue is empty → should return 0 */
    {
        uint8_t buf[8];
        uint32_t can_id = 0, is_ext = 0, is_remote = 0;
        uint32_t dlc = sim_can_recv( CAN_BUS, buf, 8, &can_id, &is_ext, &is_remote );
        sim_trace_u32( "can_rx_empty", dlc );
    }

    sim_trace_u32( "taskB_done", 1 );
}

/* ── Main entry point ────────────────────────────────────────────── */

int c_sim_can_main( void )
{
    TaskHandle_t thA, thB;
    sim_task_handle_t hA, hB;

    /* Create FreeRTOS tasks */
    xTaskCreate( vTaskA, "CanSender", 512, NULL, 2, &thA );
    xTaskCreate( vTaskB, "CanReceiver", 512, NULL, 1, &thB );

    /* Create Rust fibers */
    hA = sim_create_task( "CanSender", (sim_task_entry_fn)vTaskA, NULL, 512, 2 );
    hB = sim_create_task( "CanReceiver", (sim_task_entry_fn)vTaskB, NULL, 512, 1 );

    /* Register TCB mappings */
    sim_bridge_register( hA, thA );
    sim_bridge_register( hB, thB );

    vTaskStartScheduler();
    return 0;
}
