/* main_block.c — Virtual block device demo (Phase 38b)
 *
 * Two FreeRTOS tasks exercise FlatMemoryStore through the C ABI.
 *
 * Task A (Writer, priority 2):
 *   - Creates a block device (512-byte pages, 8 pages, 4KB total).
 *   - Writes "HELLO" at offset 0.
 *   - Writes "WORLD" at offset 100.
 *   - Reads back and verifies both.
 *
 * Task B (Reader, priority 1):
 *   - Waits, then reads both regions and verifies.
 */

#include "FreeRTOS.h"
#include "task.h"
#include "sim_abi.h"

static void writer_task(void *arg) {
    (void)arg;

    /* Create a 4KB block device: 8 pages x 512 bytes, erase=0xFF. */
    sim_block_create(0, 512, 8, 0xFF);
    sim_trace_u32("blk_created", 1);

    /* Write "HELLO" at offset 0. */
    uint8_t w1[] = {'H','E','L','L','O'};
    uint32_t n = sim_block_write(0, 0, w1, sizeof(w1));
    sim_trace_u32("blk_wrote", n);

    /* Write "WORLD" at offset 100. */
    uint8_t w2[] = {'W','O','R','L','D'};
    n = sim_block_write(0, 100, w2, sizeof(w2));
    sim_trace_u32("blk_wrote", n);

    /* Read back at offset 0. */
    uint8_t rbuf[16];
    n = sim_block_read(0, 0, rbuf, sizeof(rbuf));
    sim_trace_u32("blk_read", n);
    sim_trace_u32("blk_r0", rbuf[0]);  /* 'H' */
    sim_trace_u32("blk_r4", rbuf[4]);  /* 'O' */

    /* Read back at offset 100. */
    n = sim_block_read(0, 100, rbuf, sizeof(rbuf));
    sim_trace_u32("blk_read", n);
    sim_trace_u32("blk_r100_0", rbuf[0]);  /* 'W' */
    sim_trace_u32("blk_r100_4", rbuf[4]);  /* 'D' */

    /* Erase page 0. */
    sim_block_erase_page(0, 0);
    sim_trace_u32("blk_erased", 0);

    /* Verify erased: offset 0 should now be 0xFF. */
    n = sim_block_read(0, 0, rbuf, 1);
    sim_trace_u32("blk_read", n);
    sim_trace_u32("blk_erased_val", rbuf[0]);  /* 0xFF = 255 */

    sim_trace_u32("w_done", 0);
    vTaskDelete(NULL);
}

static void reader_task(void *arg) {
    (void)arg;

    /* Wait for writer to finish. */
    vTaskDelay(pdMS_TO_TICKS(1));

    /* Read geometry. */
    uint32_t page_size = 0, page_count = 0;
    sim_block_get_geometry(0, &page_size, &page_count);
    sim_trace_u32("blk_pg_size", page_size);
    sim_trace_u32("blk_pg_count", page_count);

    /* Verify erased byte at offset 0 (writer erased it). */
    uint8_t rbuf[16];
    uint32_t n = sim_block_read(0, 0, rbuf, 5);
    sim_trace_u32("blk_read", n);
    sim_trace_u32("blk_v0", rbuf[0]);  /* 0xFF */

    sim_trace_u32("r_done", 0);
    vTaskDelete(NULL);
}

int c_sim_block_main(void) {
    TaskHandle_t thA = NULL, thB = NULL;
    sim_task_handle_t hA, hB;

    xTaskCreate(writer_task, "wrt", configMINIMAL_STACK_SIZE, NULL, 2, &thA);
    xTaskCreate(reader_task, "rdr", configMINIMAL_STACK_SIZE, NULL, 1, &thB);

    hA = sim_create_task("wrt", (sim_task_entry_fn)writer_task, NULL, 256, 2);
    hB = sim_create_task("rdr", (sim_task_entry_fn)reader_task, NULL, 256, 1);

    sim_bridge_register(hA, thA);
    sim_bridge_register(hB, thB);

    vTaskStartScheduler();
    return 0;
}
