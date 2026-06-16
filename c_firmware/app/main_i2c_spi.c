#include "FreeRTOS.h"
#include "task.h"
#include "queue.h"
#include <stdint.h>
#include <string.h>
#include "sim_abi.h"

/* ── I2C device IDs ──────────────────────────────────────────────── */

#define I2C_BUS 0
#define SPI_BUS 0

/* ── Task A: exercises virtual I2C ────────────────────────────────── */

static void vTaskA( void *pvParameters )
{
    (void) pvParameters;

    /* Register I2C controller */
    sim_i2c_set_address( I2C_BUS, 0x50, 0 ); /* 7-bit address 0x50 */

    /* 1. Write 3 bytes to I2C target */
    {
        uint8_t cmd[] = { 0x01, 0x02, 0x03 };
        uint32_t written = sim_i2c_write( I2C_BUS, cmd, 3 );
        sim_trace_u32( "i2c_written", written );
    }

    /* 2. Inject RX data simulating target response, then read */
    {
        uint8_t response[] = { 0xAA, 0xBB, 0xCC, 0xDD };
        sim_i2c_inject_rx( I2C_BUS, response, 4 );

        uint8_t buf[4];
        uint32_t read = sim_i2c_read( I2C_BUS, buf, 4 );
        sim_trace_u32( "i2c_read_bytes", read );
        if ( read >= 1 ) sim_trace_u32( "i2c_rx_byte0", buf[0] );
    }

    /* 3. Combined write-then-read (repeated start) */
    {
        uint8_t tx[] = { 0x10, 0x00 }; /* register address */
        uint8_t response[] = { 0x42, 0x99 };
        sim_i2c_inject_rx( I2C_BUS, response, 2 );

        uint8_t rx[2];
        uint32_t read = sim_i2c_write_read( I2C_BUS, tx, 2, rx, 2 );
        sim_trace_u32( "i2c_wr_read", read );
        if ( read >= 1 ) sim_trace_u32( "i2c_wr_val", rx[0] );
    }

    /* 4. Check NACK after an operation (no NACK by default) */
    {
        uint32_t nack = sim_i2c_get_nack( I2C_BUS );
        sim_trace_u32( "i2c_nack", nack );
    }

    sim_trace_u32( "taskA_done", 1 );
}

/* ── Task B: exercises virtual SPI ────────────────────────────────── */

static void vTaskB( void *pvParameters )
{
    (void) pvParameters;

    /* 1. Configure SPI: Mode 0, 1 MHz, 8-bit */
    {
        uint32_t rc = sim_spi_set_config( SPI_BUS, 0, 1000000, 8 );
        sim_trace_u32( "spi_config", rc );
    }

    /* 2. Assert chip select */
    {
        sim_spi_set_cs( SPI_BUS, 1 );
        sim_trace_u32( "spi_cs_on", 1 );
    }

    /* 3. Full-duplex transfer: write 3 bytes, pre-load RX with 3 bytes */
    {
        uint8_t tx[] = { 0x9F, 0x00, 0x00 }; /* JEDEC ID command */
        uint8_t rx_response[] = { 0xEF, 0x40, 0x18 };
        sim_spi_inject_rx( SPI_BUS, rx_response, 3 );

        uint8_t rx[3];
        uint32_t received = sim_spi_transfer( SPI_BUS, tx, 3, rx, 3 );
        sim_trace_u32( "spi_xfer_rx", received );
        if ( received >= 1 ) sim_trace_u32( "spi_mfg_id", rx[0] );
    }

    /* 4. De-assert chip select */
    {
        sim_spi_set_cs( SPI_BUS, 0 );
        sim_trace_u32( "spi_cs_off", 1 );
    }

    /* 5. Change to SPI Mode 3 and do a write-only transfer */
    {
        sim_spi_set_config( SPI_BUS, 3, 2000000, 8 );
        sim_spi_set_cs( SPI_BUS, 1 );

        uint8_t tx[] = { 0x06 }; /* write enable */
        uint32_t rc = sim_spi_transfer( SPI_BUS, tx, 1, NULL, 0 );
        sim_trace_u32( "spi_mode3_wr", rc );
    }

    sim_trace_u32( "taskB_done", 1 );
}

/* ── Synchronization queue between tasks ─────────────────────────── */

static QueueHandle_t xSyncQueue;

/* ── Main entry point ────────────────────────────────────────────── */

int c_sim_i2c_spi_main( void )
{
    TaskHandle_t thA, thB;
    sim_task_handle_t hA, hB;

    /* Create a simple sync queue so tasks run in order */
    xSyncQueue = xQueueCreate( 1, sizeof( uint32_t ) );

    /* Create FreeRTOS tasks */
    xTaskCreate( vTaskA, "I2cTask", 512, NULL, 2, &thA );
    xTaskCreate( vTaskB, "SpiTask", 512, NULL, 2, &thB );

    /* Create Rust fibers */
    hA = sim_create_task( "I2cTask", (sim_task_entry_fn)vTaskA, NULL, 512, 2 );
    hB = sim_create_task( "SpiTask", (sim_task_entry_fn)vTaskB, NULL, 512, 2 );

    /* Register TCB mappings */
    sim_bridge_register( hA, thA );
    sim_bridge_register( hB, thB );

    vTaskStartScheduler();
    return 0;
}
