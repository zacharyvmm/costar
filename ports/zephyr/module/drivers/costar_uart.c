/*
 * UART on a costar channel device.
 *
 * Received frames are buffered and read byte by byte.  Each write (poll_out
 * or fifo_fill) is sent as one frame.  In interrupt-driven mode the device's
 * IRQ fires when a frame arrives; enabling TX pends the IRQ once, since the
 * transmitter is always ready.
 */
#define DT_DRV_COMPAT costar_uart

#include <string.h>

#include <zephyr/device.h>
#include <zephyr/drivers/uart.h>
#include <zephyr/irq.h>
#include <zephyr/logging/log.h>

#include "costar.h"

LOG_MODULE_REGISTER(costar_uart, CONFIG_UART_LOG_LEVEL);

/* native_sim's software interrupt (boards/native/native_sim/irq_handler.h). */
void posix_sw_set_pending_IRQ(unsigned int irq);

struct costar_uart_config {
	const char *costar_device;
	unsigned int irq;
	void (*irq_config)(void);
};

struct costar_uart_data {
	int channel;
	uint8_t rx[256];
	int rx_length;
	int rx_position;
#ifdef CONFIG_UART_INTERRUPT_DRIVEN
	uart_irq_callback_user_data_t callback;
	void *callback_data;
	bool rx_enabled;
	bool tx_enabled;
#endif
};

/* True if a received byte is waiting (refills from the channel). */
static bool rx_available(struct costar_uart_data *data)
{
	if (data->rx_position < data->rx_length) {
		return true;
	}
	int length = costar_chan_recv(data->channel, data->rx, sizeof(data->rx));

	if (length > (int)sizeof(data->rx)) {
		LOG_WRN("frame of %d bytes truncated", length);
		length = sizeof(data->rx);
	}
	data->rx_length = length < 0 ? 0 : length;
	data->rx_position = 0;
	return data->rx_length > 0;
}

static int costar_uart_poll_in(const struct device *dev, unsigned char *c)
{
	struct costar_uart_data *data = dev->data;

	if (!rx_available(data)) {
		return -1;
	}
	*c = data->rx[data->rx_position++];
	return 0;
}

static void costar_uart_poll_out(const struct device *dev, unsigned char c)
{
	struct costar_uart_data *data = dev->data;

	costar_chan_send(data->channel, &c, 1);
}

#ifdef CONFIG_UART_INTERRUPT_DRIVEN

static int costar_uart_fifo_fill(const struct device *dev, const uint8_t *buffer, int size)
{
	struct costar_uart_data *data = dev->data;

	return costar_chan_send(data->channel, buffer, size) == 0 ? size : 0;
}

static int costar_uart_fifo_read(const struct device *dev, uint8_t *buffer, int size)
{
	struct costar_uart_data *data = dev->data;
	int count = 0;

	while (count < size && rx_available(data)) {
		buffer[count++] = data->rx[data->rx_position++];
	}
	return count;
}

static void costar_uart_irq_tx_enable(const struct device *dev)
{
	const struct costar_uart_config *config = dev->config;
	struct costar_uart_data *data = dev->data;

	data->tx_enabled = true;
	posix_sw_set_pending_IRQ(config->irq);
}

static void costar_uart_irq_tx_disable(const struct device *dev)
{
	struct costar_uart_data *data = dev->data;

	data->tx_enabled = false;
}

static int costar_uart_irq_tx_ready(const struct device *dev)
{
	struct costar_uart_data *data = dev->data;

	return data->tx_enabled ? 1 : 0;
}

static int costar_uart_irq_tx_complete(const struct device *dev)
{
	ARG_UNUSED(dev);
	return 1;
}

static void costar_uart_irq_rx_enable(const struct device *dev)
{
	const struct costar_uart_config *config = dev->config;
	struct costar_uart_data *data = dev->data;

	data->rx_enabled = true;
	if (rx_available(data)) {
		posix_sw_set_pending_IRQ(config->irq);
	}
}

static void costar_uart_irq_rx_disable(const struct device *dev)
{
	struct costar_uart_data *data = dev->data;

	data->rx_enabled = false;
}

static int costar_uart_irq_rx_ready(const struct device *dev)
{
	struct costar_uart_data *data = dev->data;

	return data->rx_enabled && rx_available(data) ? 1 : 0;
}

static int costar_uart_irq_is_pending(const struct device *dev)
{
	return costar_uart_irq_rx_ready(dev) || costar_uart_irq_tx_ready(dev);
}

static void costar_uart_irq_update(const struct device *dev)
{
	ARG_UNUSED(dev); /* Readiness is computed on demand. */
}

static void costar_uart_irq_callback_set(const struct device *dev, uart_irq_callback_user_data_t callback,
					 void *callback_data)
{
	struct costar_uart_data *data = dev->data;

	data->callback = callback;
	data->callback_data = callback_data;
}

static void costar_uart_isr(const void *arg)
{
	const struct device *dev = arg;
	struct costar_uart_data *data = dev->data;

	if (data->callback != NULL) {
		data->callback(dev, data->callback_data);
	}
}

#endif /* CONFIG_UART_INTERRUPT_DRIVEN */

static DEVICE_API(uart, costar_uart_api) = {
	.poll_in = costar_uart_poll_in,
	.poll_out = costar_uart_poll_out,
#ifdef CONFIG_UART_INTERRUPT_DRIVEN
	.fifo_fill = costar_uart_fifo_fill,
	.fifo_read = costar_uart_fifo_read,
	.irq_tx_enable = costar_uart_irq_tx_enable,
	.irq_tx_disable = costar_uart_irq_tx_disable,
	.irq_tx_ready = costar_uart_irq_tx_ready,
	.irq_tx_complete = costar_uart_irq_tx_complete,
	.irq_rx_enable = costar_uart_irq_rx_enable,
	.irq_rx_disable = costar_uart_irq_rx_disable,
	.irq_rx_ready = costar_uart_irq_rx_ready,
	.irq_is_pending = costar_uart_irq_is_pending,
	.irq_update = costar_uart_irq_update,
	.irq_callback_set = costar_uart_irq_callback_set,
#endif
};

static int costar_uart_init(const struct device *dev)
{
	const struct costar_uart_config *config = dev->config;
	struct costar_uart_data *data = dev->data;

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
	config->irq_config();
	return 0;
}

#ifdef CONFIG_UART_INTERRUPT_DRIVEN
#define COSTAR_UART_IRQ_CONFIG(n)                                                                  \
	static void costar_uart_irq_config_##n(void)                                               \
	{                                                                                          \
		IRQ_CONNECT(DT_INST_PROP(n, irq), 1, costar_uart_isr, DEVICE_DT_INST_GET(n), 0);   \
		irq_enable(DT_INST_PROP(n, irq));                                                  \
	}
#else
#define COSTAR_UART_IRQ_CONFIG(n)                                                                  \
	static void costar_uart_irq_config_##n(void)                                               \
	{                                                                                          \
	}
#endif

#define COSTAR_UART_INIT(n)                                                                        \
	COSTAR_UART_IRQ_CONFIG(n)                                                                  \
	static const struct costar_uart_config costar_uart_config_##n = {                          \
		.costar_device = DT_INST_PROP(n, costar_device),                                   \
		.irq = DT_INST_PROP(n, irq),                                                       \
		.irq_config = costar_uart_irq_config_##n,                                          \
	};                                                                                         \
	static struct costar_uart_data costar_uart_data_##n;                                       \
	DEVICE_DT_INST_DEFINE(n, costar_uart_init, NULL, &costar_uart_data_##n,                    \
			      &costar_uart_config_##n, PRE_KERNEL_1, CONFIG_SERIAL_INIT_PRIORITY,  \
			      &costar_uart_api);

DT_INST_FOREACH_STATUS_OKAY(COSTAR_UART_INIT)
