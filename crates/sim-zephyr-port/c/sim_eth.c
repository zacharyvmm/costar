// Virtual Ethernet driver for Zephyr.
// Replaces eth_native_posix.c. Implements Zephyr's eth driver API
// using costar's VirtualEthDevice backend.

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
static uint32_t sim_eth_iface_id = 0;
static int      sim_eth_initialized = 0;

void sim_eth_init(uint32_t id, const uint8_t *mac, uint32_t mtu)
{
    sim_eth_register(id, mac, mtu);
    sim_eth_iface_id = id;
    sim_eth_initialized = 1;
}

// Zephyr eth driver: eth_iface_init(net_if) — register the interface
int sim_eth_iface_init(void *net_if)
{
    (void)net_if;
    return sim_eth_initialized ? 0 : -1;
}

// Zephyr eth driver: eth_send(net_if, pkt) — push frame to SimNetDevice
int sim_eth_iface_send(void *net_if, void *pkt)
{
    (void)net_if;
    if (!sim_eth_initialized) return -1;
    // In real Zephyr, pkt is a net_pkt; we pass the raw data.
    // For now, this is a stub that assumes pkt points to raw frame data.
    extern void *net_pkt_data(void *pkt);
    extern uint32_t net_pkt_get_len(void *pkt);
    uint8_t *data = (uint8_t *)net_pkt_data(pkt);
    uint32_t len = net_pkt_get_len(pkt);
    sim_eth_send(sim_eth_iface_id, data, len);
    return 0;
}

// Poll for incoming frames and deliver to Zephyr's net_if.
void sim_eth_recv_poll(void)
{
    if (!sim_eth_initialized) return;
    uint8_t buf[2048];
    uint32_t n;
    while ((n = sim_eth_recv(sim_eth_iface_id, buf, sizeof(buf))) > 0) {
        // In real Zephyr: allocate net_pkt, copy buf into it, call net_if_recv_data().
        // For our simulator stub, just record the delivery.
        extern void sim_eth_deliver_frame(uint8_t *data, uint32_t len);
        sim_eth_deliver_frame(buf, n);
    }
}

// Register a receive callback with the virtual device.
void sim_eth_set_recv_callback(void (*cb)(void))
{
    if (sim_eth_initialized) {
        sim_eth_on_recv(sim_eth_iface_id, cb);
    }
}

// Stub for net_pkt_data/net_pkt_get_len (the real functions are in Zephyr's net_pkt.c).
void *net_pkt_data(void *pkt) { return pkt; }
uint32_t net_pkt_get_len(void *pkt) { (void)pkt; return 0; }

// Stub for sim_eth_deliver_frame (real impl in Zephyr's net_if.c).
void sim_eth_deliver_frame(uint8_t *data, uint32_t len) { (void)data; (void)len; }
