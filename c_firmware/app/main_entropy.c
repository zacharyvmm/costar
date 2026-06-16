/*
 * main_entropy.c — Virtual entropy source demo (Phase 30).
 *
 * Two FreeRTOS tasks exercise the virtual entropy device:
 *   Task A (Collector, priority 1): requests 8 bytes of entropy from
 *     the default seed, traces the first byte, reseeds with a known
 *     value, requests 8 more bytes, traces the first byte again.
 *   Task B (Observer, priority 0): passively blocks on a queue,
 *     receives a notification, and traces completion.
 *
 * The entropy source is deterministic (seeded xorshift128+), so the trace
 * is reproducible across platforms.
 */

#include "FreeRTOS.h"
#include "task.h"
#include "queue.h"
#include "sim_abi.h"

/* ── FreeRTOS task handles ───────────────────────────────────────────── */
static TaskHandle_t xCollectorHandle = NULL;
static TaskHandle_t xObserverHandle = NULL;
static QueueHandle_t xQueue;

/* ── Entropy tasks ───────────────────────────────────────────────────── */

static void vCollector(void *pvParameters) {
    uint8_t buf[8];
    (void)pvParameters;

    /* First request: default seed */
    uint32_t n = sim_entropy_request(0, buf, 8);
    sim_trace_u32("entropy_default_first_byte", (uint32_t)buf[0]);
    sim_trace_u32("entropy_default_len", n);

    /* Reseed with a known value */
    sim_entropy_seed(0, 0xDEADBEEFCAFEBABEULL);

    /* Second request: different seed */
    n = sim_entropy_request(0, buf, 8);
    sim_trace_u32("entropy_reseeded_first_byte", (uint32_t)buf[0]);
    sim_trace_u32("entropy_reseeded_len", n);

    /* Third request: verify determinism (same seed → same output) */
    sim_entropy_seed(0, 0xDEADBEEFCAFEBABEULL);
    n = sim_entropy_request(0, buf, 8);
    sim_trace_u32("entropy_reseeded_again_first_byte", (uint32_t)buf[0]);

    sim_trace_u32("entropy_done", 1);

    /* Signal the observer. */
    xQueueSend(xQueue, "done", 0);

    /* Let the observer run. */
    vTaskDelay(1);
}

static void vObserver(void *pvParameters) {
    (void)pvParameters;

    sim_trace_u32("observer_started", 1);

    /* Block until the collector signals completion. */
    char msg[8];
    xQueueReceive(xQueue, msg, portMAX_DELAY);

    sim_trace_u32("observer_done", 1);
}

/* ── Main entry point ────────────────────────────────────────────────── */

int c_sim_entropy_main(void) {
    xQueue = xQueueCreate(5, sizeof(char[8]));

    xTaskCreate(vCollector, "Collector", 256, NULL, 1, &xCollectorHandle);
    xTaskCreate(vObserver, "Observer", 256, NULL, 0, &xObserverHandle);

    /* Create Rust fibers AFTER xTaskCreate returns. */
    sim_task_handle_t hA = sim_create_task(
        "Collector", (sim_task_entry_fn)vCollector, NULL, 256, 1);
    sim_task_handle_t hB = sim_create_task(
        "Observer", (sim_task_entry_fn)vObserver, NULL, 256, 0);
    sim_bridge_register(hA, xCollectorHandle);
    sim_bridge_register(hB, xObserverHandle);

    /* Start the scheduler — transfers control to Rust. */
    vTaskStartScheduler();

    return 0;
}
