/*
 * A CAN controller on a costar channel device.  Each CAN frame is one
 * channel frame:
 *
 *   bytes 0-3  the identifier, big-endian, with flags in the 3 bits above a
 *              29-bit identifier: 0x80000000 extended (IDE), 0x40000000
 *              remote (RTR), 0x20000000 CAN FD (FDF)
 *   byte 4     the DLC, plus 0x80 for an FD frame's bit rate switch (BRS)
 *   bytes 5-   the data: can_dlc_to_bytes(DLC) bytes, none for a remote frame
 *
 * so a wire fault's match_hex can pick frames by identifier.  Put the
 * controllers on a wire with shared_medium and the bus's bits_per_second.
 *
 * Sending queues the frame on the channel and completes at once: there are
 * no acknowledgements, arbitration losses or bus errors, and the controller
 * is always error-active while started.  Frames arrive in the device's ISR,
 * which runs the callbacks of matching filters.  In loopback mode a sent
 * frame is queued for the ISR too, as it would arrive.
 */
#define DT_DRV_COMPAT costar_can

#include <string.h>

#include <zephyr/device.h>
#include <zephyr/drivers/can.h>
#include <zephyr/irq.h>
#include <zephyr/logging/log.h>
#include <zephyr/spinlock.h>
#include <zephyr/sys/byteorder.h>

#include "costar.h"

void posix_sw_set_pending_IRQ(unsigned int irq);

LOG_MODULE_REGISTER(costar_can, CONFIG_CAN_LOG_LEVEL);

#define ID_IDE BIT(31)
#define ID_RTR BIT(30)
#define ID_FDF BIT(29)
#define DLC_BRS BIT(7)
#define HEADER 5
#define LOOPBACK_FRAMES 8 /* sent in loopback mode, not yet seen by the ISR */

#ifdef CONFIG_CAN_FD_MODE
#define FRAME_FLAGS (CAN_FRAME_IDE | CAN_FRAME_RTR | CAN_FRAME_FDF | CAN_FRAME_BRS)
#define MAX_BITRATE 8000000
#else
#define FRAME_FLAGS (CAN_FRAME_IDE | CAN_FRAME_RTR)
#define MAX_BITRATE 1000000
#endif

struct costar_can_config {
	const struct can_driver_config common; /* first: Zephyr's CAN core expects it */
	const char *costar_device;
	unsigned int irq;
	void (*irq_config)(void);
};

struct costar_can_filter {
	can_rx_callback_t callback;
	void *user_data;
	struct can_filter filter;
};

struct costar_can_data {
	struct can_driver_data common; /* first: Zephyr's CAN core expects it */
	int channel;
	struct k_spinlock lock;
	struct costar_can_filter filters[CONFIG_COSTAR_CAN_MAX_FILTERS];
	struct can_frame loopback[LOOPBACK_FRAMES]; /* a ring, under the lock */
	size_t loopback_first;
	size_t loopback_count;
};

/* Whether `frame` is one the controller can carry in mode `mode`: 0, or why not. */
static int check_frame(const struct can_frame *frame, can_mode_t mode)
{
	const bool fd = (frame->flags & CAN_FRAME_FDF) != 0;

	if ((frame->flags & ~FRAME_FLAGS) != 0) {
		return -ENOTSUP;
	}
	if (fd && (mode & CAN_MODE_FD) == 0) {
		return -ENOTSUP;
	}
	/* Bit rate switching is for FD frames, which have no remote frames. */
	if ((!fd && (frame->flags & CAN_FRAME_BRS) != 0) || (fd && (frame->flags & CAN_FRAME_RTR) != 0)) {
		return -EINVAL;
	}
	if (frame->dlc > (fd ? CANFD_MAX_DLC : CAN_MAX_DLC)) {
		return -EINVAL;
	}
	return 0;
}

static size_t encode(const struct can_frame *frame, uint8_t *out)
{
	const size_t length = (frame->flags & CAN_FRAME_RTR) != 0 ? 0 : can_dlc_to_bytes(frame->dlc);
	uint32_t id = frame->id;

	id |= (frame->flags & CAN_FRAME_IDE) != 0 ? ID_IDE : 0;
	id |= (frame->flags & CAN_FRAME_RTR) != 0 ? ID_RTR : 0;
	id |= (frame->flags & CAN_FRAME_FDF) != 0 ? ID_FDF : 0;
	sys_put_be32(id, out);
	out[4] = frame->dlc | ((frame->flags & CAN_FRAME_BRS) != 0 ? DLC_BRS : 0);
	memcpy(out + HEADER, frame->data, length);
	return HEADER + length;
}

/* False for anything that isn't a well-formed frame the controller accepts
 * in mode `mode`.
 */
static bool decode(const uint8_t *in, size_t length, can_mode_t mode, struct can_frame *frame)
{
	if (length < HEADER) {
		return false;
	}
	const uint32_t id = sys_get_be32(in);

	memset(frame, 0, sizeof(*frame));
	frame->flags |= (id & ID_IDE) != 0 ? CAN_FRAME_IDE : 0;
	frame->flags |= (id & ID_RTR) != 0 ? CAN_FRAME_RTR : 0;
	frame->flags |= (id & ID_FDF) != 0 ? CAN_FRAME_FDF : 0;
	frame->flags |= (in[4] & DLC_BRS) != 0 ? CAN_FRAME_BRS : 0;
	frame->id = id & CAN_EXT_ID_MASK;
	frame->dlc = in[4] & ~DLC_BRS;

	const size_t data = (frame->flags & CAN_FRAME_RTR) != 0 ? 0 : can_dlc_to_bytes(frame->dlc);
	const uint32_t id_mask = (id & ID_IDE) != 0 ? CAN_EXT_ID_MASK : CAN_STD_ID_MASK;

	if (check_frame(frame, mode) != 0 || length != HEADER + data ||
	    (id & ~(ID_IDE | ID_RTR | ID_FDF | id_mask)) != 0) {
		return false;
	}
	memcpy(frame->data, in + HEADER, data);
	return true;
}

/* Run the callbacks of the filters that match `frame`.  In the ISR, without
 * the lock: a callback may add or remove filters.
 */
static void dispatch(const struct device *dev, const struct can_frame *frame)
{
	struct costar_can_data *data = dev->data;

#ifndef CONFIG_CAN_ACCEPT_RTR
	if ((frame->flags & CAN_FRAME_RTR) != 0) {
		return;
	}
#endif
	for (size_t i = 0; i < ARRAY_SIZE(data->filters); i++) {
		struct costar_can_filter *filter = &data->filters[i];

		if (filter->callback != NULL && can_frame_matches_filter(frame, &filter->filter)) {
			struct can_frame copy = *frame;

			filter->callback(dev, &copy, filter->user_data);
		}
	}
}

static int costar_can_send(const struct device *dev, const struct can_frame *frame, k_timeout_t timeout,
			   can_tx_callback_t callback, void *user_data)
{
	const struct costar_can_config *config = dev->config;
	struct costar_can_data *data = dev->data;
	uint8_t bytes[HEADER + CAN_MAX_DLEN];
	const int invalid = check_frame(frame, data->common.mode);

	ARG_UNUSED(timeout);
	if (invalid != 0) {
		return invalid;
	}
	if (!data->common.started) {
		return -ENETDOWN;
	}
	if ((data->common.mode & CAN_MODE_LOOPBACK) != 0) {
		k_spinlock_key_t key = k_spin_lock(&data->lock);

		if (data->loopback_count == LOOPBACK_FRAMES) {
			k_spin_unlock(&data->lock, key);
			return -EAGAIN; /* interrupts locked for eight sends */
		}
		data->loopback[(data->loopback_first + data->loopback_count++) % LOOPBACK_FRAMES] = *frame;
		k_spin_unlock(&data->lock, key);
		posix_sw_set_pending_IRQ(config->irq);
	}
	costar_chan_send(data->channel, bytes, encode(frame, bytes));
	callback(dev, 0, user_data);
	return 0;
}

static int costar_can_add_rx_filter(const struct device *dev, can_rx_callback_t callback, void *user_data,
				    const struct can_filter *filter)
{
	struct costar_can_data *data = dev->data;
	int id = -ENOSPC;

	if ((filter->flags & ~CAN_FILTER_IDE) != 0) {
		return -ENOTSUP;
	}

	k_spinlock_key_t key = k_spin_lock(&data->lock);

	for (size_t i = 0; i < ARRAY_SIZE(data->filters); i++) {
		if (data->filters[i].callback == NULL) {
			data->filters[i] = (struct costar_can_filter){callback, user_data, *filter};
			id = (int)i;
			break;
		}
	}
	k_spin_unlock(&data->lock, key);
	return id;
}

static void costar_can_remove_rx_filter(const struct device *dev, int id)
{
	struct costar_can_data *data = dev->data;

	if (id < 0 || id >= (int)ARRAY_SIZE(data->filters)) {
		return;
	}

	k_spinlock_key_t key = k_spin_lock(&data->lock);

	data->filters[id].callback = NULL;
	k_spin_unlock(&data->lock, key);
}

static int costar_can_get_capabilities(const struct device *dev, can_mode_t *capabilities)
{
	ARG_UNUSED(dev);
	*capabilities = CAN_MODE_NORMAL | CAN_MODE_LOOPBACK;
	IF_ENABLED(CONFIG_CAN_FD_MODE, (*capabilities |= CAN_MODE_FD;))
	return 0;
}

static int costar_can_start(const struct device *dev)
{
	struct costar_can_data *data = dev->data;

	if (data->common.started) {
		return -EALREADY;
	}
	data->common.started = true;
	return 0;
}

static int costar_can_stop(const struct device *dev)
{
	struct costar_can_data *data = dev->data;

	if (!data->common.started) {
		return -EALREADY;
	}

	k_spinlock_key_t key = k_spin_lock(&data->lock);

	data->common.started = false;
	data->loopback_count = 0; /* aborted, as frames waiting to be sent */
	k_spin_unlock(&data->lock, key);
	return 0;
}

static int costar_can_set_mode(const struct device *dev, can_mode_t mode)
{
	struct costar_can_data *data = dev->data;
	can_mode_t supported;

	costar_can_get_capabilities(dev, &supported);
	if (data->common.started) {
		return -EBUSY;
	}
	if ((mode & ~supported) != 0) {
		return -ENOTSUP;
	}
	data->common.mode = mode;
	return 0;
}

static int costar_can_set_timing(const struct device *dev, const struct can_timing *timing)
{
	struct costar_can_data *data = dev->data;

	ARG_UNUSED(timing); /* the wire's bits_per_second sets the timing */
	return data->common.started ? -EBUSY : 0;
}

static int costar_can_get_state(const struct device *dev, enum can_state *state,
				struct can_bus_err_cnt *err_cnt)
{
	struct costar_can_data *data = dev->data;

	if (state != NULL) {
		*state = data->common.started ? CAN_STATE_ERROR_ACTIVE : CAN_STATE_STOPPED;
	}
	if (err_cnt != NULL) {
		err_cnt->tx_err_cnt = 0;
		err_cnt->rx_err_cnt = 0;
	}
	return 0;
}

static int costar_can_get_core_clock(const struct device *dev, uint32_t *rate)
{
	ARG_UNUSED(dev);
	*rate = MHZ(80); /* recommended by CiA 601-3 */
	return 0;
}

static int costar_can_get_max_filters(const struct device *dev, bool ide)
{
	ARG_UNUSED(dev);
	ARG_UNUSED(ide);
	return CONFIG_COSTAR_CAN_MAX_FILTERS;
}

static void costar_can_isr(const void *arg)
{
	const struct device *dev = arg;
	struct costar_can_data *data = dev->data;
	uint8_t bytes[HEADER + CAN_MAX_DLEN];
	struct can_frame frame;
	int length;

	for (;;) {
		k_spinlock_key_t key = k_spin_lock(&data->lock);
		const bool looped = data->loopback_count > 0;

		if (looped) {
			frame = data->loopback[data->loopback_first];
			data->loopback_first = (data->loopback_first + 1) % LOOPBACK_FRAMES;
			data->loopback_count--;
		}
		k_spin_unlock(&data->lock, key);
		if (!looped) {
			break;
		}
		if (data->common.started) {
			dispatch(dev, &frame);
		}
	}
	while ((length = costar_chan_recv(data->channel, bytes, sizeof(bytes))) >= 0) {
		if (!data->common.started) {
			continue;
		}
		if (!decode(bytes, (size_t)length, data->common.mode, &frame)) {
			LOG_WRN("dropped a malformed frame of %d bytes", length);
			continue;
		}
		dispatch(dev, &frame);
	}
}

#define TIMING_LIMITS                                                                              \
	.timing_min = {.sjw = 1, .prop_seg = 0, .phase_seg1 = 2, .phase_seg2 = 2, .prescaler = 1},  \
	.timing_max = {.sjw = 128, .prop_seg = 0, .phase_seg1 = 256, .phase_seg2 = 128,             \
		       .prescaler = 32}

static DEVICE_API(can, costar_can_api) = {
	.get_capabilities = costar_can_get_capabilities,
	.start = costar_can_start,
	.stop = costar_can_stop,
	.set_mode = costar_can_set_mode,
	.set_timing = costar_can_set_timing,
	.send = costar_can_send,
	.add_rx_filter = costar_can_add_rx_filter,
	.remove_rx_filter = costar_can_remove_rx_filter,
	.get_state = costar_can_get_state,
	.get_core_clock = costar_can_get_core_clock,
	.get_max_filters = costar_can_get_max_filters,
	TIMING_LIMITS,
#ifdef CONFIG_CAN_FD_MODE
	.set_timing_data = costar_can_set_timing,
	.timing_data_min = {.sjw = 1, .prop_seg = 0, .phase_seg1 = 1, .phase_seg2 = 1, .prescaler = 1},
	.timing_data_max = {.sjw = 16, .prop_seg = 0, .phase_seg1 = 32, .phase_seg2 = 16,
			    .prescaler = 32},
#endif
};

static int costar_can_init(const struct device *dev)
{
	const struct costar_can_config *config = dev->config;
	struct costar_can_data *data = dev->data;

	sys_slist_init(&data->common.state_change_callbacks);
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

#define COSTAR_CAN_INIT(n)                                                                         \
	static void costar_can_irq_config_##n(void)                                                \
	{                                                                                          \
		IRQ_CONNECT(DT_INST_PROP(n, irq), 1, costar_can_isr, DEVICE_DT_INST_GET(n), 0);    \
		irq_enable(DT_INST_PROP(n, irq));                                                  \
	}                                                                                          \
	static const struct costar_can_config costar_can_config_##n = {                            \
		.common = CAN_DT_DRIVER_CONFIG_INST_GET(n, 0, MAX_BITRATE),                        \
		.costar_device = DT_INST_PROP(n, costar_device),                                   \
		.irq = DT_INST_PROP(n, irq),                                                       \
		.irq_config = costar_can_irq_config_##n,                                           \
	};                                                                                         \
	static struct costar_can_data costar_can_data_##n;                                         \
	CAN_DEVICE_DT_INST_DEFINE(n, costar_can_init, NULL, &costar_can_data_##n,                  \
				  &costar_can_config_##n, POST_KERNEL, CONFIG_CAN_INIT_PRIORITY,   \
				  &costar_can_api);

DT_INST_FOREACH_STATUS_OKAY(COSTAR_CAN_INIT)
