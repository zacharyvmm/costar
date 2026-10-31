/*
 * An I2C controller on a costar channel device.  Each i2c_transfer() is one
 * request frame, answered by one reply frame (include/costar_i2c.h); on the
 * host, device models answer for the bus's parts.
 *
 * A transfer sends its request and blocks the calling thread until the
 * reply arrives (the device's ISR takes it), or until the transfer timeout
 * (zephyr,transfer-timeout-ms, else CONFIG_I2C_TRANSFER_TIMEOUT_MS; 0 waits
 * for ever).  A NACK, a timeout or a malformed reply fails the transfer with
 * -EIO.  Each request carries a tag, so a reply that arrives too late is
 * dropped rather than taken for the next transfer's.  The bus speed doesn't
 * change timing: the wire's bits_per_second does.  Controller mode only.
 */
#define DT_DRV_COMPAT costar_i2c

#include <string.h>

#include <zephyr/device.h>
#include <zephyr/drivers/i2c.h>
#include <zephyr/dt-bindings/i2c/i2c.h>
#include <zephyr/irq.h>
#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>
#include <zephyr/spinlock.h>
#include <zephyr/sys/byteorder.h>

#include "costar.h"
#include "costar_i2c.h"

LOG_MODULE_REGISTER(costar_i2c, CONFIG_I2C_LOG_LEVEL);

#define MSG_FLAGS (I2C_MSG_RW_MASK | I2C_MSG_STOP | I2C_MSG_RESTART | I2C_MSG_ADDR_10_BITS)

BUILD_ASSERT(I2C_MSG_READ == COSTAR_I2C_MSG_READ && I2C_MSG_STOP == COSTAR_I2C_MSG_STOP &&
	     I2C_MSG_RESTART == COSTAR_I2C_MSG_RESTART);

struct costar_i2c_config {
	const char *costar_device;
	unsigned int irq;
	uint32_t bitrate;
	k_timeout_t timeout;
	void (*irq_config)(void);
};

struct costar_i2c_data {
	int channel;
	uint32_t dev_config;
	struct k_mutex transfer; /* one transfer at a time */
	struct k_sem replied;
	struct k_spinlock lock;
	/* Under the lock: the reply a transfer waits for, and its length once
	 * it has arrived (in `frame`).
	 */
	bool waiting;
	uint16_t address;
	uint8_t tag;
	int reply_length;
	uint8_t frame[CONFIG_COSTAR_I2C_FRAME_SIZE]; /* the request, then the reply */
};

/* The I2C_SPEED_* nearest above `bitrate` (as Zephyr's controllers map clock-frequency). */
static uint32_t speed(uint32_t bitrate)
{
	if (bitrate <= I2C_BITRATE_STANDARD) {
		return I2C_SPEED_SET(I2C_SPEED_STANDARD);
	} else if (bitrate <= I2C_BITRATE_FAST) {
		return I2C_SPEED_SET(I2C_SPEED_FAST);
	} else if (bitrate <= I2C_BITRATE_FAST_PLUS) {
		return I2C_SPEED_SET(I2C_SPEED_FAST_PLUS);
	} else if (bitrate <= I2C_BITRATE_HIGH) {
		return I2C_SPEED_SET(I2C_SPEED_HIGH);
	}
	return I2C_SPEED_SET(I2C_SPEED_ULTRA);
}

static int costar_i2c_configure(const struct device *dev, uint32_t dev_config)
{
	struct costar_i2c_data *data = dev->data;

	if ((dev_config & I2C_MODE_CONTROLLER) == 0) {
		return -ENOTSUP;
	}
	k_mutex_lock(&data->transfer, K_FOREVER);
	data->dev_config = dev_config;
	k_mutex_unlock(&data->transfer);
	return 0;
}

static int costar_i2c_get_config(const struct device *dev, uint32_t *dev_config)
{
	struct costar_i2c_data *data = dev->data;

	*dev_config = data->dev_config;
	return 0;
}

/* Encode the request in data->frame: its length, or -EINVAL if it doesn't fit. */
static int encode(struct costar_i2c_data *data, const struct i2c_msg *msgs, uint8_t num_msgs, uint16_t address,
		  size_t *reply_length)
{
	size_t length = COSTAR_I2C_REQUEST_HEADER;

	*reply_length = COSTAR_I2C_REPLY_HEADER;
	sys_put_be16(address, data->frame);
	data->frame[2] = data->tag;
	for (uint8_t i = 0; i < num_msgs; i++) {
		const struct i2c_msg *msg = &msgs[i];
		const bool read = (msg->flags & I2C_MSG_READ) != 0;
		const size_t size = COSTAR_I2C_MESSAGE_HEADER + (read ? 0 : msg->len);

		if (msg->len > COSTAR_I2C_MAX_MESSAGE || size > sizeof(data->frame) - length) {
			return -EINVAL;
		}
		data->frame[length] = msg->flags & (I2C_MSG_RW_MASK | I2C_MSG_STOP | I2C_MSG_RESTART);
		sys_put_be16(msg->len, &data->frame[length + 1]);
		if (!read) {
			memcpy(&data->frame[length + COSTAR_I2C_MESSAGE_HEADER], msg->buf, msg->len);
		}
		length += size;
		*reply_length += read ? msg->len : 0;
	}
	return *reply_length > sizeof(data->frame) ? -EINVAL : (int)length;
}

static int costar_i2c_transfer(const struct device *dev, struct i2c_msg *msgs, uint8_t num_msgs, uint16_t addr)
{
	const struct costar_i2c_config *config = dev->config;
	struct costar_i2c_data *data = dev->data;
	bool ten_bit = false;
	size_t expected;
	int ret;

	if (num_msgs == 0) {
		return 0;
	}
	for (uint8_t i = 0; i < num_msgs; i++) {
		if ((msgs[i].flags & ~MSG_FLAGS) != 0) {
			return -ENOTSUP;
		}
		ten_bit |= (msgs[i].flags & I2C_MSG_ADDR_10_BITS) != 0;
	}
	if (addr > (ten_bit ? 0x3ff : 0x7f)) {
		return -EINVAL;
	}
	if (ten_bit) {
		addr |= COSTAR_I2C_ADDR_10_BITS;
	}

	k_mutex_lock(&data->transfer, K_FOREVER);

	k_spinlock_key_t key = k_spin_lock(&data->lock);

	data->tag++;
	ret = encode(data, msgs, num_msgs, addr, &expected);
	if (ret < 0) {
		k_spin_unlock(&data->lock, key);
		k_mutex_unlock(&data->transfer);
		LOG_ERR("a transfer to 0x%x doesn't fit in CONFIG_COSTAR_I2C_FRAME_SIZE", addr);
		return ret;
	}
	data->waiting = true;
	data->address = addr;
	data->reply_length = -1;
	k_sem_reset(&data->replied);
	costar_chan_send(data->channel, data->frame, (size_t)ret);
	k_spin_unlock(&data->lock, key);

	const int waited = k_sem_take(&data->replied, config->timeout);

	key = k_spin_lock(&data->lock);
	data->waiting = false;
	const int length = data->reply_length;

	k_spin_unlock(&data->lock, key);

	if (waited != 0 || length < 0) {
		LOG_DBG("no reply from 0x%x", addr);
		ret = -EIO;
	} else if (data->frame[3] != COSTAR_I2C_ACK) {
		LOG_DBG("0x%x: status %u", addr, data->frame[3]);
		ret = -EIO;
	} else if ((size_t)length != expected) {
		LOG_WRN("0x%x: a reply of %d bytes, not %zu", addr, length, expected);
		ret = -EIO;
	} else {
		const uint8_t *in = &data->frame[COSTAR_I2C_REPLY_HEADER];

		for (uint8_t i = 0; i < num_msgs; i++) {
			if ((msgs[i].flags & I2C_MSG_READ) != 0) {
				memcpy(msgs[i].buf, in, msgs[i].len);
				in += msgs[i].len;
			}
		}
		ret = 0;
	}
	k_mutex_unlock(&data->transfer);
	return ret;
}

static void costar_i2c_isr(const void *arg)
{
	const struct device *dev = arg;
	struct costar_i2c_data *data = dev->data;
	k_spinlock_key_t key = k_spin_lock(&data->lock);

	for (;;) {
		if (!data->waiting) {
			if (costar_chan_recv(data->channel, NULL, 0) < 0) { /* a late reply: drop it */
				break;
			}
			continue;
		}
		const int length = costar_chan_recv(data->channel, data->frame, sizeof(data->frame));

		if (length < 0) {
			break;
		}
		if (length >= COSTAR_I2C_REPLY_HEADER && (size_t)length <= sizeof(data->frame) &&
		    sys_get_be16(data->frame) == data->address && data->frame[2] == data->tag) {
			data->waiting = false;
			data->reply_length = length;
			k_sem_give(&data->replied);
		}
	}
	k_spin_unlock(&data->lock, key);
}

static DEVICE_API(i2c, costar_i2c_api) = {
	.configure = costar_i2c_configure,
	.get_config = costar_i2c_get_config,
	.transfer = costar_i2c_transfer,
};

static int costar_i2c_init(const struct device *dev)
{
	const struct costar_i2c_config *config = dev->config;
	struct costar_i2c_data *data = dev->data;

	k_mutex_init(&data->transfer);
	k_sem_init(&data->replied, 0, 1);
	data->dev_config = I2C_MODE_CONTROLLER | speed(config->bitrate);
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

#define COSTAR_I2C_INIT(n)                                                                         \
	static void costar_i2c_irq_config_##n(void)                                                \
	{                                                                                          \
		IRQ_CONNECT(DT_INST_PROP(n, irq), 1, costar_i2c_isr, DEVICE_DT_INST_GET(n), 0);    \
		irq_enable(DT_INST_PROP(n, irq));                                                  \
	}                                                                                          \
	static const struct costar_i2c_config costar_i2c_config_##n = {                            \
		.costar_device = DT_INST_PROP(n, costar_device),                                   \
		.irq = DT_INST_PROP(n, irq),                                                       \
		.bitrate = DT_INST_PROP_OR(n, clock_frequency, I2C_BITRATE_STANDARD),              \
		.timeout = I2C_DT_INST_TRANSFER_TIMEOUT(n),                                        \
		.irq_config = costar_i2c_irq_config_##n,                                           \
	};                                                                                         \
	static struct costar_i2c_data costar_i2c_data_##n;                                         \
	I2C_DEVICE_DT_INST_DEFINE(n, costar_i2c_init, NULL, &costar_i2c_data_##n,                  \
				  &costar_i2c_config_##n, POST_KERNEL, CONFIG_I2C_INIT_PRIORITY,   \
				  &costar_i2c_api);

DT_INST_FOREACH_STATUS_OKAY(COSTAR_I2C_INIT)
