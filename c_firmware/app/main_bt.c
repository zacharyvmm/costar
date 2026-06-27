/* main_bt.c — Virtual HCI controller demo (Phase 38c)
 *
 * Two FreeRTOS tasks exercise VirtualHciController through the C ABI.
 *
 * Task A (BT Host, priority 2):
 *   - Registers HCI controller (id=0).
 *   - Sends HCI Reset command.
 *   - Injects a CommandComplete response.
 *   - Receives and verifies it.
 *
 * Task B (BT Observer, priority 1):
 *   - Waits, then sends an ACL data packet.
 *   - Injects a disconnect event.
 *   - Receives and counts both.
 */

#include "FreeRTOS.h"
#include "task.h"
#include "sim_abi.h"

/* HCI packet types per Bluetooth spec */
#define HCI_CMD  1
#define HCI_ACL  2
#define HCI_EVT  4

static void bt_host_task(void *arg) {
    (void)arg;

    /* Register a virtual HCI controller. */
    sim_bt_register(0);
    sim_trace_u32("bt_reg", 1);

    /* Send HCI Reset command (OGF=0x03, OCF=0x0003 → opcode 0x0C03). */
    uint8_t cmd[] = {0x03, 0x0C, 0x00};  /* opcode LE, param len 0 */
    sim_bt_send(0, HCI_CMD, cmd, sizeof(cmd));
    sim_trace_u32("bt_cmd_sent", sizeof(cmd));

    /* Inject a CommandComplete(0x0E) for HCI_Reset. */
    uint8_t evt[] = {0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00};
    /* event=CommandComplete(0x0E), param_len=4, num_hci_cmd_pkts=1, opcode=0x0C03, status=0 */
    sim_bt_inject_event(0, evt, sizeof(evt));
    sim_trace_u32("bt_evt_inj", sizeof(evt));

    /* Receive the event. */
    vTaskDelay(pdMS_TO_TICKS(1));
    uint8_t pkt_type = 0;
    uint8_t buf[64];
    uint32_t n = sim_bt_recv(0, &pkt_type, buf, sizeof(buf));
    sim_trace_u32("bt_recv", n);
    sim_trace_u32("bt_pkt_type", pkt_type);
    sim_trace_u32("bt_evt_code", buf[0]);  /* should be 0x0E */

    sim_trace_u32("h_done", 0);
    vTaskDelete(NULL);
}

static void bt_observer_task(void *arg) {
    (void)arg;

    /* Wait for host to finish registration. */
    vTaskDelay(pdMS_TO_TICKS(1));

    /* Send an ACL data packet (connection handle 0, PB=2, BC=0, len=3). */
    uint8_t acl[] = {0x00, 0x00, 0x03, 0x00, 0x41, 0x42, 0x43};
    /* handle=0, PB=2(L2CAP start), len=3 → "ABC" */
    sim_bt_send(0, HCI_ACL, acl, sizeof(acl));
    sim_trace_u32("bt_acl_sent", sizeof(acl));

    /* Inject a disconnect complete event (0x05). */
    uint8_t disc[] = {0x05, 0x04, 0x00, 0x00, 0x00, 0x13};
    /* DisconnectComplete(0x05), status=0, handle=0, reason=0x13 */
    sim_bt_inject_event(0, disc, sizeof(disc));
    sim_trace_u32("bt_disc_inj", sizeof(disc));

    /* Receive ACL data (from peer perspective, queued by inject). */
    vTaskDelay(pdMS_TO_TICKS(1));
    uint8_t pkt_type = 0;
    uint8_t buf[64];
    uint32_t n = sim_bt_recv(0, &pkt_type, buf, sizeof(buf));
    sim_trace_u32("bt_recv", n);

    sim_trace_u32("o_done", 0);
    vTaskDelete(NULL);
}

int c_sim_bt_main(void) {
    TaskHandle_t thA = NULL, thB = NULL;
    sim_task_handle_t hA, hB;

    xTaskCreate(bt_host_task, "hst", configMINIMAL_STACK_SIZE, NULL, 2, &thA);
    xTaskCreate(bt_observer_task, "obs", configMINIMAL_STACK_SIZE, NULL, 1, &thB);

    hA = sim_create_task("hst", (sim_task_entry_fn)bt_host_task, NULL, 256, 2);
    hB = sim_create_task("obs", (sim_task_entry_fn)bt_observer_task, NULL, 256, 1);

    sim_bridge_register(hA, thA);
    sim_bridge_register(hB, thB);

    vTaskStartScheduler();
    return 0;
}
