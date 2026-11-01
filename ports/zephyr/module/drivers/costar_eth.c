/*
 * Ethernet on a costar channel device: each channel frame is one Ethernet
 * frame, from the destination address to the end of the payload, with no
 * preamble or FCS and no padding to 60 bytes (as on a Linux TAP interface,
 * so a scenario can bridge the channel to one).
 *
 * Sending copies the packet into one frame and sends it at once; the wire
 * models the link's latency and speed.  A frame arriving raises the
 * device's IRQ, and the ISR copies every waiting frame into a packet for
 * the driver's RX thread, which hands it to the network stack
 * (net_recv_data()).  That may process it at once, in the caller, and take
 * mutexes (with CONFIG_NET_TC_RX_COUNT=0, or a high-priority packet with
 * CONFIG_NET_TC_RX_SKIP_FOR_HIGH_PRIO), so it can't be called from the ISR.
 * Frames that arrive while the interface is stopped, frames larger than
 * NET_ETH_MAX_FRAME_SIZE and frames the stack has no buffer for are dropped.
 *
 * The link is up from the start: there is no PHY.  The MAC address comes
 * from devicetree (local-mac-address, zephyr,random-mac-address or an NVMEM
 * cell); without one, it is random, as with zephyr,random-mac-address.
 * costar,entropy makes that deterministic, so machines that run the same
 * image need different entropy seeds to get different addresses.
 */
#define DT_DRV_COMPAT costar_eth

#include <string.h>

#include <zephyr/device.h>
#include <zephyr/irq.h>
#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>
#include <zephyr/net/ethernet.h>
#include <zephyr/net/net_if.h>
#include <zephyr/net/net_pkt.h>
#include <zephyr/random/random.h>

#include "costar.h"

LOG_MODULE_REGISTER(costar_eth, CONFIG_ETHERNET_LOG_LEVEL);

struct costar_eth_config {
	const char *costar_device;
	unsigned int irq;
	void (*irq_config)(void);
	struct net_eth_mac_config mac;
};

struct costar_eth_data {
	int channel;
	struct net_if *iface;
	uint8_t mac[NET_ETH_ADDR_LEN];
	bool started;
	uint8_t rx[NET_ETH_MAX_FRAME_SIZE]; /* the ISR's */
	struct k_fifo rx_queue;             /* packets from the ISR, for the RX thread */
	struct k_thread rx_thread;
	K_KERNEL_STACK_MEMBER(rx_stack, CONFIG_COSTAR_ETH_RX_THREAD_STACK_SIZE);
	struct k_mutex tx_lock;
	uint8_t tx[NET_ETH_MAX_FRAME_SIZE]; /* under tx_lock */
};

static int costar_eth_send(const struct device *dev, struct net_pkt *pkt)
{
	struct costar_eth_data *data = dev->data;
	const size_t length = net_pkt_get_len(pkt);
	int status = 0;

	if (length > sizeof(data->tx)) {
		return -EMSGSIZE;
	}
	k_mutex_lock(&data->tx_lock, K_FOREVER);
	if (net_pkt_read(pkt, data->tx, length) != 0 || costar_chan_send(data->channel, data->tx, length) != 0) {
		status = -EIO;
	}
	k_mutex_unlock(&data->tx_lock);
	return status;
}

static void costar_eth_isr(const void *arg)
{
	const struct device *dev = arg;
	struct costar_eth_data *data = dev->data;
	int length;

	while ((length = costar_chan_recv(data->channel, data->rx, sizeof(data->rx))) >= 0) {
		if (!data->started) {
			continue;
		}
		if (length > (int)sizeof(data->rx)) {
			LOG_WRN("dropped a frame of %d bytes", length);
			continue;
		}
		struct net_pkt *pkt =
			net_pkt_rx_alloc_with_buffer(data->iface, length, NET_AF_UNSPEC, 0, K_NO_WAIT);

		if (pkt == NULL) {
			LOG_WRN("no buffer for a frame of %d bytes", length);
			continue;
		}
		if (net_pkt_write(pkt, data->rx, length) != 0) {
			net_pkt_unref(pkt);
			continue;
		}
		k_fifo_put(&data->rx_queue, pkt);
	}
}

static void costar_eth_rx_thread(void *arg1, void *arg2, void *arg3)
{
	struct costar_eth_data *data = arg1;

	ARG_UNUSED(arg2);
	ARG_UNUSED(arg3);
	for (;;) {
		struct net_pkt *pkt = k_fifo_get(&data->rx_queue, K_FOREVER);

		if (net_recv_data(data->iface, pkt) < 0) {
			net_pkt_unref(pkt);
		}
	}
}

static void costar_eth_iface_init(struct net_if *iface)
{
	const struct device *dev = net_if_get_device(iface);
	const struct costar_eth_config *config = dev->config;
	struct costar_eth_data *data = dev->data;

	data->iface = iface;
	ethernet_init(iface);
	if (net_eth_mac_load(&config->mac, data->mac) == -ENODATA) {
		/* As zephyr,random-mac-address: a unicast, locally administered address. */
		sys_rand_get(data->mac, sizeof(data->mac));
		data->mac[0] = (data->mac[0] & ~0x01) | 0x02;
	}
	net_if_set_link_addr(iface, data->mac, sizeof(data->mac), NET_LINK_ETHERNET);
	k_thread_create(&data->rx_thread, data->rx_stack, K_KERNEL_STACK_SIZEOF(data->rx_stack),
			costar_eth_rx_thread, data, NULL, NULL,
			K_PRIO_COOP(CONFIG_COSTAR_ETH_RX_THREAD_PRIORITY), 0, K_NO_WAIT);
	k_thread_name_set(&data->rx_thread, "costar_eth_rx");
	config->irq_config();
}

static int costar_eth_start(const struct device *dev, struct net_if *iface)
{
	struct costar_eth_data *data = dev->data;

	ARG_UNUSED(iface);
	data->started = true;
	return 0;
}

static int costar_eth_stop(const struct device *dev, struct net_if *iface)
{
	struct costar_eth_data *data = dev->data;

	ARG_UNUSED(iface);
	data->started = false;
	return 0;
}

static enum ethernet_hw_caps costar_eth_get_capabilities(const struct device *dev, struct net_if *iface)
{
	ARG_UNUSED(dev);
	ARG_UNUSED(iface);
	return ETHERNET_LINK_100BASE | (IS_ENABLED(CONFIG_NET_PROMISCUOUS_MODE) ? ETHERNET_PROMISC_MODE : 0);
}

static int costar_eth_set_config(const struct device *dev, struct net_if *iface, enum ethernet_config_type type,
				 const struct ethernet_config *config)
{
	struct costar_eth_data *data = dev->data;

	ARG_UNUSED(iface);
	switch (type) {
	case ETHERNET_CONFIG_TYPE_MAC_ADDRESS:
		memcpy(data->mac, config->mac_address.addr, sizeof(data->mac));
		return 0;
	case ETHERNET_CONFIG_TYPE_PROMISC_MODE:
		/* The device has no address filter: every frame reaches the stack. */
		return 0;
	default:
		return -ENOTSUP;
	}
}

static const struct ethernet_api costar_eth_api = {
	.iface_api.init = costar_eth_iface_init,
	.start = costar_eth_start,
	.stop = costar_eth_stop,
	.get_capabilities = costar_eth_get_capabilities,
	.set_config = costar_eth_set_config,
	.send = costar_eth_send,
};

static int costar_eth_init(const struct device *dev)
{
	const struct costar_eth_config *config = dev->config;
	struct costar_eth_data *data = dev->data;

	data->channel = costar_dev(config->costar_device);
	if (data->channel < 0) {
		LOG_ERR("the board has no channel named %s", config->costar_device);
		return -ENODEV;
	}
	if (costar_dev_irq(data->channel) != (int)config->irq) {
		LOG_ERR("%s raises IRQ %d on the board, not %u", config->costar_device,
			costar_dev_irq(data->channel), config->irq);
		return -EINVAL;
	}
	k_mutex_init(&data->tx_lock);
	k_fifo_init(&data->rx_queue);
	return 0;
}

#define COSTAR_ETH_INIT(n)                                                                         \
	static void costar_eth_irq_config_##n(void)                                                \
	{                                                                                          \
		IRQ_CONNECT(DT_INST_PROP(n, irq), 1, costar_eth_isr, DEVICE_DT_INST_GET(n), 0);    \
		irq_enable(DT_INST_PROP(n, irq));                                                  \
	}                                                                                          \
	static const struct costar_eth_config costar_eth_config_##n = {                            \
		.costar_device = DT_INST_PROP(n, costar_device),                                   \
		.irq = DT_INST_PROP(n, irq),                                                       \
		.irq_config = costar_eth_irq_config_##n,                                           \
		.mac = NET_ETH_MAC_DT_INST_CONFIG_INIT(n),                                         \
	};                                                                                         \
	static struct costar_eth_data costar_eth_data_##n;                                         \
	ETH_NET_DEVICE_DT_INST_DEFINE(n, costar_eth_init, NULL, &costar_eth_data_##n,              \
				      &costar_eth_config_##n, CONFIG_ETH_INIT_PRIORITY,            \
				      &costar_eth_api, NET_ETH_MTU);

DT_INST_FOREACH_STATUS_OKAY(COSTAR_ETH_INIT)
