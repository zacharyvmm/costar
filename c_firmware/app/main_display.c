/**
 * @file main_display.c
 * @brief Virtual display and touch screen firmware demo.
 *
 * Exercises:
 *   - sim_display_init / sim_display_enable / sim_display_set_backlight
 *   - sim_display_fill_rect — filled rectangles in RGB565 colors
 *   - sim_display_set_pixel — individual pixel writes
 *   - sim_touch_init — touch screen initialization
 *
 * Uses FreeRTOS tasks running on corosensei fibers.
 * Traces key operations with sim_trace_u32 for deterministic golden-trace verification.
 */

#include "FreeRTOS.h"
#include "task.h"
#include "queue.h"
#include "sim_abi.h"

/* ── Shared queue for task signalling ───────────────────────────── */
static QueueHandle_t xDrawQueue;

/* ── Task A (Drawer): initialises display and draws shapes ──────── */
static void vTaskDraw(void *pvParameters) {
    (void)pvParameters;

    /* ── Trace: task started ───────────────────────────────────── */
    sim_trace_u32("draw_task_start", 1);

    /* Init display: id=0, 320×240, RGB565 */
    uint32_t rc = sim_display_init(0, 320, 240, 0);
    sim_trace_u32("display_init", rc);

    /* Init touch screen associated with display 0 */
    rc = sim_touch_init(0, 0);
    sim_trace_u32("touch_init", rc);

    /* Enable the display */
    sim_display_enable(0, 1);
    sim_trace_u32("display_enabled", 1);

    /* Set backlight to 80% */
    sim_display_set_backlight(0, 80);
    sim_trace_u32("backlight_set", 80);

    /* Clear screen — fill entire 320×240 with black (0x0000) */
    sim_display_fill_rect(0, 0, 0, 320, 240, 0x0000);
    sim_trace_u32("clear_screen", 1);

    /* Red rectangle: top-left quadrant */
    sim_display_fill_rect(0, 10, 10, 100, 50, 0xF800);
    sim_trace_u32("red_rect", 0xF800);

    /* Green rectangle: top-right quadrant */
    sim_display_fill_rect(0, 120, 10, 100, 50, 0x07E0);
    sim_trace_u32("green_rect", 0x07E0);

    /* Blue rectangle: bottom-left quadrant */
    sim_display_fill_rect(0, 10, 70, 100, 50, 0x001F);
    sim_trace_u32("blue_rect", 0x001F);

    /* White rectangle: bottom-right quadrant */
    sim_display_fill_rect(0, 120, 70, 100, 50, 0xFFFF);
    sim_trace_u32("white_rect", 0xFFFF);

    /* Draw some individual pixels */
    sim_display_set_pixel(0, 50, 100, 0xF800);   /* red pixel */
    sim_trace_u32("pixel_red", 0xF800);
    sim_display_set_pixel(0, 150, 100, 0x07E0);  /* green pixel */
    sim_trace_u32("pixel_green", 0x07E0);
    sim_display_set_pixel(0, 100, 50, 0x001F);   /* blue pixel */
    sim_trace_u32("pixel_blue", 0x001F);

    /* Dim the backlight (fade effect) */
    sim_display_set_backlight(0, 40);
    sim_trace_u32("backlight_dim", 40);

    /* Disable the display */
    sim_display_enable(0, 0);
    sim_trace_u32("display_disabled", 1);

    /* Signal the watcher task */
    uint32_t done = 1;
    xQueueSend(xDrawQueue, &done, 0);
    vTaskDelay(1);

    sim_trace_u32("draw_task_done", 1);
}

/* ── Task B (Watcher): waits for draw signal ────────────────────── */
static void vTaskWatch(void *pvParameters) {
    uint32_t sig;
    (void)pvParameters;

    if (xQueueReceive(xDrawQueue, &sig, 0) == pdPASS) {
        sim_trace_u32("watch_done", sig);
        return;
    }

    /* Spin briefly waiting for the signal */
    int retries = 0;
    while (retries < 5) {
        vTaskDelay(1);
        if (xQueueReceive(xDrawQueue, &sig, 0) == pdPASS) {
            sim_trace_u32("watch_done", sig);
            return;
        }
        retries++;
    }
    sim_trace_u32("watch_timeout", 1);
}

/* ── FreeRTOS memory stubs ──────────────────────────────────────── */
/* vApplicationGetIdleTaskMemory / vApplicationGetTimerTaskMemory are provided
 * once by main.c, which is always compiled into the same embedded_c_payload
 * archive (see sim-freertos-port/build.rs).  Defining them here as well makes
 * both translation units export the same strong symbols, which any modern
 * linker (lld and GNU ld alike) rejects as a duplicate definition when the
 * whole payload is linked into a binary.  Rely on main.c's definitions. */

/* ── Entry point called from Rust ───────────────────────────────── */
int c_sim_display_main(void) {
    TaskHandle_t thDraw, thWatch;

    xDrawQueue = xQueueCreate(5, sizeof(uint32_t));

    /* Create FreeRTOS tasks */
    xTaskCreate(vTaskDraw, "Drawer",  256, NULL, 1, &thDraw);
    xTaskCreate(vTaskWatch, "Watcher", 256, NULL, 1, &thWatch);

    /* Create Rust fibers directly (not via trace hook) */
    sim_task_handle_t hDraw = sim_create_task(
        "Drawer",
        (sim_task_entry_fn)vTaskDraw,
        NULL, 256, 1
    );
    sim_task_handle_t hWatch = sim_create_task(
        "Watcher",
        (sim_task_entry_fn)vTaskWatch,
        NULL, 256, 1
    );

    /* Register TCB mappings for sim_set_current_task_by_id */
    sim_bridge_register(hDraw, thDraw);
    sim_bridge_register(hWatch, thWatch);

    vTaskStartScheduler();
    return 0;
}
