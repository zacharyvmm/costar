/*
 * FreeRTOS+TCP integration demo for costar simulator.
 *
 * Simpler test: registers NIC, sends/receives frames via the
 * smoltcp bridge in a single task without timing dependencies.
 *
 * Compiled only when SIM_TCP=1.
 */

#include "FreeRTOS.h"
#include "task.h"
#include "queue.h"
#include "FreeRTOS_IP.h"
#include "FreeRTOS_Sockets.h"
#include "FreeRTOS_Routing.h"
#include "sim_abi.h"

extern NetworkInterface_t *sim_nic_init(uint32_t eth_id, const uint8_t *mac);
extern void sim_nic_poll_receive(void);

static const uint8_t ucIPAddress[4]  = { 10, 0, 0, 2 };
static const uint8_t ucNetMask[4]    = { 255, 255, 255, 0 };
static const uint8_t ucGateway[4]    = { 10, 0, 0, 1 };
static const uint8_t ucDNSServer[4]  = { 10, 0, 0, 1 };
static const uint8_t ucMACAddress[6] = { 0x02, 0x00, 0x00, 0x00, 0x00, 0x02 };

/* ── Single integration test task ───────────────────────────────── */

static void vTestTask(void *pvParameters)
{
    (void) pvParameters;
    sim_trace_u32("tcp:task_start", 0);

    /* Step 1: register the Ethernet device via the C ABI (bypasses FreeRTOS+TCP's NIC init). */
    sim_eth_register(0, ucMACAddress, 1500);
    sim_trace_u32("tcp:eth_registered", 0);

    /* Step 2: send a broadcast ARP request for 10.0.0.1.
     * The smoltcp bridge at 10.0.0.1 should reply. */
    uint8_t arp[] = {
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,  /* dst MAC: broadcast */
        0x02, 0x00, 0x00, 0x00, 0x00, 0x02,  /* src MAC: ours */
        0x08, 0x06,                            /* EtherType: ARP */
        /* ARP header */
        0x00, 0x01,                            /* hw type: Ethernet */
        0x08, 0x00,                            /* proto: IPv4 */
        0x06,                                  /* hw addr len: 6 */
        0x04,                                  /* proto addr len: 4 */
        0x00, 0x01,                            /* op: request */
        /* sender hw addr */
        0x02, 0x00, 0x00, 0x00, 0x00, 0x02,
        /* sender proto addr: 10.0.0.2 */
        0x0A, 0x00, 0x00, 0x02,
        /* target hw addr: unknown */
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        /* target proto addr: 10.0.0.1 */
        0x0A, 0x00, 0x00, 0x01
    };
    sim_eth_send(0, arp, sizeof(arp));
    sim_trace_u32("tcp:arp_sent", sizeof(arp));

    /* Step 3: yield so the scheduler runs eth_loopback_bridge(). */
    sim_port_yield();

    /* Step 4: poll for response. */
    uint8_t buf[256];
    if (sim_eth_poll(0)) {
        uint32_t n = sim_eth_recv(0, buf, sizeof(buf));
        sim_trace_u32("tcp:frame_rcvd", n);
    } else {
        sim_trace_u32("tcp:no_frame", 0);
    }

    /* Step 5: send again, yield, poll again. */
    sim_eth_send(0, arp, sizeof(arp));
    sim_trace_u32("tcp:arp_sent2", sizeof(arp));
    vTaskDelay(pdMS_TO_TICKS(1));

    if (sim_eth_poll(0)) {
        uint32_t n = sim_eth_recv(0, buf, sizeof(buf));
        sim_trace_u32("tcp:frame_rcvd2", n);
    } else {
        sim_trace_u32("tcp:no_frame2", 0);
    }

    sim_trace_u32("tcp:task_done", 0);
    vTaskDelete(NULL);
}

/* ── Callbacks required by FreeRTOS+TCP ──────────────────────────── */

uint32_t ulApplicationGetNextSequenceNumber(uint32_t a, uint16_t b, uint32_t c, uint16_t d)
{
    (void)a; (void)b; (void)c; (void)d;
    static uint32_t n = 0x12345678;
    return n++;
}

BaseType_t xApplicationGetRandomNumber(uint32_t *pv)
{
    static uint32_t r = 0xDEADBEEF;
    r = r * 1103515245 + 12345;
    *pv = r;
    return pdPASS;
}

void vApplicationPingReplyHook(ePingReplyStatus_t s, uint16_t id)
{
    (void)s;
    sim_trace_u32("tcp:ping_hook", (uint32_t)id);
}

/* ── Entry point ────────────────────────────────────────────────── */

void c_sim_tcp_echo_main(void)
{
    sim_trace_u32("tcp:boot", 0);

    /* Register the network interface with FreeRTOS+TCP. */
    NetworkInterface_t *px = sim_nic_init(0, ucMACAddress);
    sim_trace_u32("tcp:nic_init", px ? 1 : 0);

    /* Start IP stack (creates IP-task). */
    FreeRTOS_IPInit(ucIPAddress, ucNetMask, ucGateway, ucDNSServer, ucMACAddress);
    sim_trace_u32("tcp:ip_init", 0);

    /* Create test task. */
    TaskHandle_t th = NULL;
    xTaskCreate(vTestTask, "Test",
        configMINIMAL_STACK_SIZE * 4, NULL,
        3, &th);
    sim_trace_u32("tcp:task_created", 0);

    /* Create fiber. */
    sim_task_handle_t h = sim_create_task(
        "Test", (sim_task_entry_fn)vTestTask, NULL, 256, 3);
    sim_bridge_register(h, th);
    sim_trace_u32("tcp:fiber_created", 0);

    vTaskStartScheduler();
}
