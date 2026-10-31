/*
 * A SPI controller on a costar channel device.  Each spi_transceive() is a
 * transaction on the chip select its spi_config names (a device's `reg`):
 * frames that clock its bytes through the device, each answered by a reply
 * frame with the bytes received (include/costar_spi.h).  On the host, device
 * models answer for the bus's devices.
 *
 * A transceive clocks max(tx, rx) bytes: the tx buffers, then 0x00 (also
 * for NULL tx buffers); received bytes for NULL rx buffers are dropped.
 * Transfers longer than a frame are split, within one transaction.  The
 * calling thread blocks until each reply arrives (the device's ISR takes
 * it), or for CONFIG_SPI_COMPLETION_TIMEOUT_TOLERANCE ms, when the
 * transceive fails with -ETIMEDOUT.  Each request carries a tag, so a reply
 * that arrives too late is dropped rather than taken for the next one's.
 *
 * SPI_LOCK_ON keeps the bus for one spi_config until spi_release();
 * SPI_HOLD_ON_CS keeps its chip select active until the next transceive
 * without it, or spi_release().  A failed transceive may leave its chip
 * select active on the far side (when a reply was lost, not its request),
 * so it releases it at once, with a frame of its own, before it gives up
 * the bus; if that fails too, spi_release() tries again (and returns its
 * error), and the next transceive starts a transaction afresh anyway.
 *
 * Controller mode, 8-bit words, MSB first, single line, full duplex; chip
 * selects are native (no cs-gpios).  The frequency doesn't change timing:
 * the wire's bits_per_second does.
 */
#define DT_DRV_COMPAT costar_spi

#include <string.h>

#include <zephyr/device.h>
#include <zephyr/drivers/spi.h>
#include <zephyr/irq.h>
#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>
#include <zephyr/spinlock.h>
#include <zephyr/sys/byteorder.h>

#include "costar.h"
#include "costar_spi.h"

LOG_MODULE_REGISTER(costar_spi, CONFIG_SPI_LOG_LEVEL);

#define CHUNK MIN(CONFIG_COSTAR_SPI_FRAME_SIZE - COSTAR_SPI_REQUEST_HEADER, COSTAR_SPI_MAX_LENGTH)

BUILD_ASSERT(CONFIG_COSTAR_SPI_FRAME_SIZE > COSTAR_SPI_REQUEST_HEADER);

struct costar_spi_config {
	const char *costar_device;
	unsigned int irq;
	void (*irq_config)(void);
};

struct costar_spi_data {
	int channel;
	struct k_sem bus;                /* taken by a transceive; kept with SPI_LOCK_ON */
	const struct spi_config *owner;  /* the spi_config that holds the bus */
	bool held;                       /* the active one is held for the next transceive */
	bool active;                     /* a chip select may be active on the far side: */
	uint8_t active_chip_select;
	struct k_sem replied;
	struct k_spinlock lock;
	/* Under the lock: the reply a frame waits for, and its length once it
	 * has arrived (in `frame`).
	 */
	bool waiting;
	uint8_t chip_select;
	uint8_t tag;
	int reply_length;
	uint8_t frame[CONFIG_COSTAR_SPI_FRAME_SIZE]; /* a request, then its reply */
};

/* Copy `length` bytes of the buffers' stream, from `offset`, into `out`;
 * 0x00 past their end and for NULL buffers.
 */
static void gather(const struct spi_buf_set *set, size_t offset, uint8_t *out, size_t length)
{
	memset(out, 0, length);
	for (size_t i = 0; set != NULL && i < set->count && length > 0; i++) {
		const struct spi_buf *buf = &set->buffers[i];

		if (offset >= buf->len) {
			offset -= buf->len;
			continue;
		}
		const size_t n = MIN(buf->len - offset, length);

		if (buf->buf != NULL) {
			memcpy(out, (const uint8_t *)buf->buf + offset, n);
		}
		out += n;
		length -= n;
		offset = 0;
	}
}

/* Copy `in` into the buffers' stream from `offset`, skipping NULL buffers. */
static void scatter(const struct spi_buf_set *set, size_t offset, const uint8_t *in, size_t length)
{
	for (size_t i = 0; set != NULL && i < set->count && length > 0; i++) {
		const struct spi_buf *buf = &set->buffers[i];

		if (offset >= buf->len) {
			offset -= buf->len;
			continue;
		}
		const size_t n = MIN(buf->len - offset, length);

		if (buf->buf != NULL) {
			memcpy((uint8_t *)buf->buf + offset, in, n);
		}
		in += n;
		length -= n;
		offset = 0;
	}
}

static size_t total(const struct spi_buf_set *set)
{
	size_t length = 0;

	for (size_t i = 0; set != NULL && i < set->count; i++) {
		length += set->buffers[i].len;
	}
	return length;
}

/* Send one frame and wait for its reply: 0, or a negative errno. */
static int exchange(struct costar_spi_data *data, uint8_t chip_select, uint8_t flags, size_t length,
		    size_t wanted, size_t sent)
{
	k_spinlock_key_t key = k_spin_lock(&data->lock);

	data->tag++;
	data->frame[0] = chip_select;
	data->frame[1] = data->tag;
	data->frame[2] = flags;
	sys_put_be16(length, &data->frame[3]);
	sys_put_be16(wanted, &data->frame[5]);
	data->waiting = true;
	data->chip_select = chip_select;
	data->reply_length = -1;
	k_sem_reset(&data->replied);
	costar_chan_send(data->channel, data->frame, COSTAR_SPI_REQUEST_HEADER + sent);
	k_spin_unlock(&data->lock, key);

	const int waited = k_sem_take(&data->replied, K_MSEC(CONFIG_SPI_COMPLETION_TIMEOUT_TOLERANCE));

	key = k_spin_lock(&data->lock);
	data->waiting = false;
	const int reply_length = data->reply_length;

	k_spin_unlock(&data->lock, key);

	if (waited != 0 || reply_length < 0) {
		LOG_DBG("no reply on chip select %u", chip_select);
		return -ETIMEDOUT;
	}
	if (data->frame[2] != COSTAR_SPI_OK || (size_t)reply_length != COSTAR_SPI_REPLY_HEADER + wanted) {
		LOG_WRN("chip select %u: a bad reply (status %u, %d bytes)", chip_select, data->frame[2],
			reply_length);
		return -EIO;
	}
	return 0;
}

static int check_config(const struct spi_config *config)
{
	if (SPI_OP_MODE_GET(config->operation) != SPI_OP_MODE_CONTROLLER ||
	    SPI_WORD_SIZE_GET(config->operation) != 8 || (config->operation & SPI_TRANSFER_LSB) != 0 ||
	    (config->operation & SPI_MODE_LOOP) != 0 || (config->operation & SPI_LINES_MASK) != SPI_LINES_SINGLE ||
	    spi_cs_is_gpio(config) || config->peripheral > UINT8_MAX) {
		return -ENOTSUP;
	}
	return 0;
}

/* Release the chip select that may be active, while holding the bus: 0, or
 * a negative errno (it may still be active).
 */
static int deselect(struct costar_spi_data *data)
{
	int ret = 0;

	if (data->active) {
		ret = exchange(data, data->active_chip_select, COSTAR_SPI_DESELECT, 0, 0, 0);
		data->active = ret != 0;
	}
	data->held = false;
	return ret;
}

static int costar_spi_transceive(const struct device *dev, const struct spi_config *config,
				 const struct spi_buf_set *tx_bufs, const struct spi_buf_set *rx_bufs)
{
	struct costar_spi_data *data = dev->data;
	const size_t tx = total(tx_bufs);
	const size_t rx = total(rx_bufs);
	const size_t length = MAX(tx, rx);
	const uint8_t chip_select = (uint8_t)config->peripheral;
	int ret = check_config(config);

	if (ret != 0) {
		return ret;
	}
	if (data->owner != config || (config->operation & SPI_LOCK_ON) == 0) {
		k_sem_take(&data->bus, K_FOREVER);
		data->owner = config;
	}

	/* The first frame starts a transaction, unless the chip select is
	 * still held from the last; the last one ends it, unless held.
	 */
	size_t offset = 0;
	bool first = true;

	do {
		const size_t chunk = MIN(length - offset, CHUNK);
		const bool last = offset + chunk == length;
		const size_t wanted = rx > offset ? MIN(rx - offset, chunk) : 0;
		const size_t sent = tx > offset ? MIN(tx - offset, chunk) : 0;
		uint8_t flags = 0;

		flags |= first && !(data->held && data->active_chip_select == chip_select) ? COSTAR_SPI_SELECT
										      : 0;
		flags |= last && (config->operation & SPI_HOLD_ON_CS) == 0 ? COSTAR_SPI_DESELECT : 0;
		gather(tx_bufs, offset, &data->frame[COSTAR_SPI_REQUEST_HEADER], sent);
		ret = exchange(data, chip_select, flags, chunk, wanted, sent);
		if (ret != 0) {
			break;
		}
		scatter(rx_bufs, offset, &data->frame[COSTAR_SPI_REPLY_HEADER], wanted);
		offset += chunk;
		first = false;
	} while (offset < length);

	data->active_chip_select = chip_select;
	if (ret == 0) {
		data->held = (config->operation & SPI_HOLD_ON_CS) != 0;
		data->active = data->held;
	} else {
		/* The far side may have seen a frame without DESELECT, or this
		 * one's reply was lost: release the chip select now (a part
		 * acts on its command), not when the next transaction selects.
		 * The next transaction selects afresh in any case.
		 */
		data->active = true;
		(void)deselect(data);
	}
	if (ret != 0 || (config->operation & SPI_LOCK_ON) == 0) {
		data->owner = NULL;
		k_sem_give(&data->bus);
	}
	return ret;
}

static int costar_spi_release(const struct device *dev, const struct spi_config *config)
{
	struct costar_spi_data *data = dev->data;
	int ret;

	if (data->owner != config) {
		k_sem_take(&data->bus, K_FOREVER);
	}
	ret = deselect(data); /* on failure, the next release tries again */
	data->owner = NULL;
	k_sem_give(&data->bus);
	return ret;
}

static void costar_spi_isr(const void *arg)
{
	const struct device *dev = arg;
	struct costar_spi_data *data = dev->data;
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
		if (length >= COSTAR_SPI_REPLY_HEADER && (size_t)length <= sizeof(data->frame) &&
		    data->frame[0] == data->chip_select && data->frame[1] == data->tag) {
			data->waiting = false;
			data->reply_length = length;
			k_sem_give(&data->replied);
		}
	}
	k_spin_unlock(&data->lock, key);
}

static DEVICE_API(spi, costar_spi_api) = {
	.transceive = costar_spi_transceive,
	.release = costar_spi_release,
};

static int costar_spi_init(const struct device *dev)
{
	const struct costar_spi_config *config = dev->config;
	struct costar_spi_data *data = dev->data;

	k_sem_init(&data->bus, 1, 1);
	k_sem_init(&data->replied, 0, 1);
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

#define COSTAR_SPI_INIT(n)                                                                         \
	static void costar_spi_irq_config_##n(void)                                                \
	{                                                                                          \
		IRQ_CONNECT(DT_INST_PROP(n, irq), 1, costar_spi_isr, DEVICE_DT_INST_GET(n), 0);    \
		irq_enable(DT_INST_PROP(n, irq));                                                  \
	}                                                                                          \
	static const struct costar_spi_config costar_spi_config_##n = {                            \
		.costar_device = DT_INST_PROP(n, costar_device),                                   \
		.irq = DT_INST_PROP(n, irq),                                                       \
		.irq_config = costar_spi_irq_config_##n,                                           \
	};                                                                                         \
	static struct costar_spi_data costar_spi_data_##n;                                         \
	SPI_DEVICE_DT_INST_DEFINE(n, costar_spi_init, NULL, &costar_spi_data_##n,                  \
				  &costar_spi_config_##n, POST_KERNEL, CONFIG_SPI_INIT_PRIORITY,   \
				  &costar_spi_api);

DT_INST_FOREACH_STATUS_OKAY(COSTAR_SPI_INIT)
