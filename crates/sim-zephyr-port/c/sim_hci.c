// Virtual HCI driver for Zephyr.
// Replaces hci_uart.c. Registers with Zephyr's HCI core via bt_hci_driver_register().

#include <stdint.h>
#include <stddef.h>
#include <string.h>

// Forward-declare the C ABI functions.
extern uint32_t sim_bt_register(uint32_t id);
extern void     sim_bt_send(uint32_t id, uint8_t packet_type,
                             const uint8_t *data, uint32_t len);
extern uint32_t sim_bt_recv(uint32_t id, uint8_t *packet_type,
                             uint8_t *buf, uint32_t buf_size);
extern void     sim_bt_inject_event(uint32_t id, const uint8_t *data, uint32_t len);
extern void     sim_bt_on_recv(uint32_t id, void (*callback)(void));

// Driver state
static uint32_t sim_hci_ctrl_id = 0;
static int      sim_hci_initialized = 0;

void sim_hci_init(uint32_t id)
{
    sim_bt_register(id);
    sim_hci_ctrl_id = id;
    sim_hci_initialized = 1;
}

// Zephyr HCI: bt_send(pkt) — send command/ACL data to controller
int sim_hci_send(void *pkt)
{
    (void)pkt;
    if (!sim_hci_initialized) return -1;
    // In real Zephyr, pkt is a bt_buf with HCI type in first byte.
    // For now, this is a stub.
    extern uint8_t *bt_buf_get_data(void *pkt);
    extern uint32_t bt_buf_get_len(void *pkt);
    uint8_t *data = bt_buf_get_data(pkt);
    uint32_t len = bt_buf_get_len(pkt);
    if (len > 0) {
        uint8_t pkt_type = data[0];
        sim_bt_send(sim_hci_ctrl_id, pkt_type, data + 1, len - 1);
    }
    return 0;
}

// Poll for incoming HCI events/data from the controller and deliver to host.
void sim_hci_recv_poll(void)
{
    if (!sim_hci_initialized) return;
    uint8_t pkt_type;
    uint8_t buf[1024];
    uint32_t n;
    while ((n = sim_bt_recv(sim_hci_ctrl_id, &pkt_type, buf, sizeof(buf))) > 0) {
        // In real Zephyr: allocate bt_buf, copy buf into it, call bt_recv().
        extern void sim_hci_deliver_to_host(uint8_t pkt_type, uint8_t *data, uint32_t len);
        sim_hci_deliver_to_host(pkt_type, buf, n);
    }
}

void sim_hci_set_recv_callback(void (*cb)(void))
{
    if (sim_hci_initialized) {
        sim_bt_on_recv(sim_hci_ctrl_id, cb);
    }
}

// Stubs for Zephyr bt_buf functions (real impl in Zephyr's buf.c).
uint8_t *bt_buf_get_data(void *pkt) { return (uint8_t *)pkt; }
uint32_t bt_buf_get_len(void *pkt) { (void)pkt; return 0; }

void sim_hci_deliver_to_host(uint8_t pkt_type, uint8_t *data, uint32_t len)
{
    (void)pkt_type; (void)data; (void)len;
}
