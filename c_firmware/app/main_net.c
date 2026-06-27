/* main_net.c — Virtual Ethernet device loopback demo (Phase 38a) */

#include "FreeRTOS.h"
#include "task.h"
#include "sim_abi.h"

static void sender_task(void *arg) {
    (void)arg;
    uint8_t mac[6] = {0x02,0x00,0x00,0x00,0x00,0x01};
    sim_eth_register(0, mac, 1500);
    sim_trace_u32("eth_reg", 1);
    uint8_t f1[] = {0xFF,0xFF,0xFF,0xFF,0xFF,0xFF, 0x02,0x00,0x00,0x00,0x00,0x01, 0x08,0x00, 0x01};
    sim_eth_send(0, f1, sizeof(f1));
    sim_trace_u32("eth_sent", sizeof(f1));
    vTaskDelay(pdMS_TO_TICKS(1));
    uint8_t f2[] = {0xFF,0xFF,0xFF,0xFF,0xFF,0xFF, 0x02,0x00,0x00,0x00,0x00,0x01, 0x08,0x00, 0x0A,0x0B};
    sim_eth_send(0, f2, sizeof(f2));
    sim_trace_u32("eth_sent", sizeof(f2));
    sim_trace_u32("s_done", 0);
    vTaskDelete(NULL);
}

static void receiver_task(void *arg) {
    (void)arg;
    uint8_t buf[256];
    for (int i = 0; i < 5; i++) {
        if (sim_eth_poll(0)) {
            uint32_t n = sim_eth_recv(0, buf, sizeof(buf));
            sim_trace_u32("eth_recv", n);
        }
        vTaskDelay(pdMS_TO_TICKS(1));
    }
    sim_trace_u32("r_done", 0);
    vTaskDelete(NULL);
}

int c_sim_net_main(void) {
    TaskHandle_t thA = NULL, thB = NULL;
    sim_task_handle_t hA, hB;

    /* Create FreeRTOS TCBs first. */
    xTaskCreate(sender_task, "snd", configMINIMAL_STACK_SIZE, NULL, 2, &thA);
    xTaskCreate(receiver_task, "rcv", configMINIMAL_STACK_SIZE, NULL, 1, &thB);

    /* Create Rust fibers directly (like deterministic mode). */
    hA = sim_create_task("snd", (sim_task_entry_fn)sender_task, NULL, 256, 2);
    hB = sim_create_task("rcv", (sim_task_entry_fn)receiver_task, NULL, 256, 1);

    /* Register TCB mappings for sim_set_current_task_by_id. */
    sim_bridge_register(hA, thA);
    sim_bridge_register(hB, thB);

    vTaskStartScheduler();
    return 0;
}
