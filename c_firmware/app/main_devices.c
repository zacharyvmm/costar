/**
 * @file main_devices.c
 * @brief Combined demo for virtual sensors, storage, and fault injection.
 *
 * Exercises:
 *   - ADC: inject reading on channel 0, read it back
 *   - Temperature sensor: set value, read it back
 *   - EEPROM: write two bytes, read them back
 *   - Flash: erase page, write data, read it back
 *   - Fault injection: inject I2C NACK, verify injection
 *
 * Uses FreeRTOS tasks running on corosensei fibers.
 */

#include "FreeRTOS.h"
#include "task.h"
#include "queue.h"
#include "sim_abi.h"

/* ── Shared queue for task communication ─────────────────────────── */
static QueueHandle_t xQueue;

/* ── Task A (Producer): exercises sensors, storage, and fault inject ─ */
static void vTaskA(void *pvParameters) {
    (void)pvParameters;

    /* ── ADC ───────────────────────────────────────────────────── */
    uint16_t adc_val = sim_adc_read(0, 0);
    sim_trace_u32("adc_ch0_read", adc_val);

    /* ── Temperature sensor ────────────────────────────────────── */
    int32_t temp = sim_temp_read(0);
    sim_trace_u32("temp_read", (uint32_t)temp);

    /* ── EEPROM ────────────────────────────────────────────────── */
    /* Write two bytes starting at address 0 */
    sim_eeprom_write(0, 0, 0xAA);
    sim_eeprom_write(0, 1, 0x55);
    /* Read them back */
    uint32_t byte0 = sim_eeprom_read(0, 0);
    uint32_t byte1 = sim_eeprom_read(0, 1);
    sim_trace_u32("eeprom_byte0", byte0);
    sim_trace_u32("eeprom_byte1", byte1);

    /* ── Flash ─────────────────────────────────────────────────── */
    /* Erase page 0, then write 4 bytes at offset 0 */
    sim_flash_erase(0, 0);
    uint8_t flash_data[] = { 0xDE, 0xAD, 0xBE, 0xEF };
    sim_flash_write(0, 0, 0, flash_data, 4);
    /* Read back the first two bytes */
    uint32_t fb0 = sim_flash_read(0, 0);
    uint32_t fb1 = sim_flash_read(0, 1);
    sim_trace_u32("flash_byte0", fb0);
    sim_trace_u32("flash_byte1", fb1);

    /* Inject an I2C NACK fault for the next read attempt */
    sim_fault_inject_i2c_nack();
    sim_trace_u32("fault_i2c_nack_injected", 1);

    /* Signal the receiver */
    xQueueSend(xQueue, "done", 0);
    vTaskDelay(1);

    /* ADC read again (unchanged) */
    adc_val = sim_adc_read(0, 0);
    sim_trace_u32("adc_ch0_read2", adc_val);

    /* Temperature read again (unchanged) */
    temp = sim_temp_read(0);
    sim_trace_u32("temp_read2", (uint32_t)temp);

    /* Clear all faults */
    sim_fault_clear();
    sim_trace_u32("faults_cleared", 1);

    /* Done — let task function return (don't call vTaskDelete). */
}

/* ── Task B (Consumer): waits for the signal ──────────────────────── */
static void vTaskB(void *pvParameters) {
    (void)pvParameters;

    char buf[8];
    xQueueReceive(xQueue, buf, portMAX_DELAY);
    sim_trace_u32("consumer_done", 1);

    /* Done — let task function return. */
}

/* ── Entry point called from Rust ─────────────────────────────────── */
int c_sim_devices_main(void) {
    TaskHandle_t thA, thB;

    /* Pre-inject an ADC reading for channel 0 */
    sim_adc_inject_reading(0, 0, 2048);  /* half of 12-bit range */

    /* Set temperature to 30.5 °C (30500 milli-degrees) */
    sim_temp_set_value(0, 30500);

    xQueue = xQueueCreate(5, sizeof(char[8]));

    xTaskCreate(vTaskA, "Producer", 256, NULL, 1, &thA);
    xTaskCreate(vTaskB, "Consumer", 256, NULL, 1, &thB);

    /* Create Rust fibers AFTER xTaskCreate returns. */
    sim_task_handle_t hA = sim_create_task("Producer", (sim_task_entry_fn)vTaskA, NULL, 256, 1);
    sim_task_handle_t hB = sim_create_task("Consumer", (sim_task_entry_fn)vTaskB, NULL, 256, 1);
    sim_bridge_register(hA, thA);
    sim_bridge_register(hB, thB);

    vTaskStartScheduler();

    /* Should never reach here */
    return 0;
}
