// Virtual Ethernet driver for FreeRTOS+TCP.
// Implements NetworkInterface_t using costar's VirtualEthDevice backend.

#include <stdint.h>
#include <stddef.h>
#include <string.h>

// Forward-declare the C ABI functions.
extern uint32_t sim_eth_register(uint32_t id, const uint8_t *mac, uint32_t mtu);
extern uint32_t sim_eth_send(uint32_t id, const uint8_t *data, uint32_t len);
extern uint32_t sim_eth_recv(uint32_t id, uint8_t *buf, uint32_t buf_size);
extern uint32_t sim_eth_poll(uint32_t id);
extern void     sim_eth_on_recv(uint32_t id, void (*callback)(void));

// Driver state
static uint32_t sim_fr_eth_id = 0;
static int      sim_fr_eth_initialized = 0;

void sim_fr_eth_init(uint32_t id, const uint8_t *mac, uint32_t mtu)
{
    sim_eth_register(id, mac, mtu);
    sim_fr_eth_id = id;
    sim_fr_eth_initialized = 1;
}

// FreeRTOS+TCP: pxOutputFunction — push frame to VirtualEthDevice
int sim_fr_eth_output(void *buf, uint32_t len)
{
    if (!sim_fr_eth_initialized) return -1;
    sim_eth_send(sim_fr_eth_id, (const uint8_t *)buf, len);
    return 0;
}

// FreeRTOS+TCP: pxGetPhyLinkStatus — link always up
int sim_fr_eth_link_status(void)
{
    return sim_fr_eth_initialized ? 1 : 0;
}

// Deliver incoming frames to FreeRTOS+TCP.
// In real FreeRTOS+TCP, this would call eConsiderFrameForProcessing().
void sim_fr_eth_poll(void)
{
    if (!sim_fr_eth_initialized) return;
    uint8_t buf[2048];
    uint32_t n;
    while ((n = sim_eth_recv(sim_fr_eth_id, buf, sizeof(buf))) > 0) {
        // Stub: in real FreeRTOS+TCP, call eConsiderFrameForProcessing(buf, n).
        extern void sim_fr_eth_deliver_frame(uint8_t *data, uint32_t len);
        sim_fr_eth_deliver_frame(buf, n);
    }
}

void sim_fr_eth_set_recv_callback(void (*cb)(void))
{
    if (sim_fr_eth_initialized) {
        sim_eth_on_recv(sim_fr_eth_id, cb);
    }
}

void sim_fr_eth_deliver_frame(uint8_t *data, uint32_t len) { (void)data; (void)len; }
