/*
 * An ADC on a costar registers device: register n holds channel n's raw
 * sample, set by the host or a plant.
 *
 * A conversion takes no virtual time: a sampling reads the registers of the
 * sequence's channels at once, so adc_read() returns at the instant it was
 * called (with no sequence interval).  Samples are 16 bits wide; a register
 * above the resolution's full scale reads as full scale, as an input above
 * the reference does.
 *
 * The register is the converter's output, so the analog front end (gain,
 * reference, acquisition time) is the scenario's to model: those settings
 * are accepted and don't change the sample.  Oversampling averages samples
 * that are all the same, and calibration has nothing to do.  Differential
 * channels aren't supported.
 *
 * Sequence options work as on any Zephyr ADC (adc_context.h): extra
 * samplings, callbacks, and an interval, timed by a kernel timer, so each
 * sampling reads the registers at its own virtual time.  Samplings run in
 * the caller's thread, or in the timer's ISR with an interval.
 */
#define DT_DRV_COMPAT costar_adc

#include <zephyr/device.h>
#include <zephyr/drivers/adc.h>
#include <zephyr/logging/log.h>
#include <zephyr/sys/atomic.h>
#include <zephyr/sys/util.h>

#include "costar.h"

#define ADC_CONTEXT_USES_KERNEL_TIMER
#include "adc_context.h"

LOG_MODULE_REGISTER(costar_adc, CONFIG_ADC_LOG_LEVEL);

#define MAX_RESOLUTION 16

struct costar_adc_config {
	const char *costar_device;
	uint8_t channels;
};

struct costar_adc_data {
	struct adc_context ctx;
	const struct device *dev;
	int registers;
	atomic_t configured;     /* channels set up */
	uint16_t full_scale;     /* of the sequence being read */
	uint16_t *buffer;        /* where the next sampling goes */
	uint16_t *repeat_buffer; /* where the last one went */
	bool sampling;           /* a sampling loop is running */
	bool again;              /* ... and should run another */
};

static int costar_adc_channel_setup(const struct device *dev, const struct adc_channel_cfg *channel_cfg)
{
	const struct costar_adc_config *config = dev->config;
	struct costar_adc_data *data = dev->data;

	if (channel_cfg->channel_id >= config->channels) {
		LOG_ERR("no channel %u", channel_cfg->channel_id);
		return -EINVAL;
	}
	if (channel_cfg->differential) {
		return -ENOTSUP;
	}
	atomic_or(&data->configured, BIT(channel_cfg->channel_id));
	return 0;
}

static int start_read(const struct device *dev, const struct adc_sequence *sequence)
{
	struct costar_adc_data *data = dev->data;
	size_t needed = __builtin_popcount(sequence->channels) * sizeof(uint16_t);

	if (sequence->channels == 0 || (sequence->channels & ~(uint32_t)atomic_get(&data->configured)) != 0) {
		LOG_ERR("channels 0x%x aren't all set up", sequence->channels);
		return -EINVAL;
	}
	if (sequence->resolution == 0 || sequence->resolution > MAX_RESOLUTION) {
		return -ENOTSUP;
	}
	if (sequence->options != NULL) {
		needed *= 1 + sequence->options->extra_samplings;
	}
	if (sequence->buffer_size < needed) {
		return -ENOMEM;
	}
	data->full_scale = BIT_MASK(sequence->resolution);
	data->buffer = sequence->buffer;
	adc_context_start_read(&data->ctx, sequence);
	return adc_context_wait_for_completion(&data->ctx);
}

static int read_common(const struct device *dev, const struct adc_sequence *sequence, bool asynchronous,
		       struct k_poll_signal *signal)
{
	struct costar_adc_data *data = dev->data;

	adc_context_lock(&data->ctx, asynchronous, signal);
	const int status = start_read(dev, sequence);

	adc_context_release(&data->ctx, status);
	return status;
}

static int costar_adc_read(const struct device *dev, const struct adc_sequence *sequence)
{
	return read_common(dev, sequence, false, NULL);
}

#ifdef CONFIG_ADC_ASYNC
static int costar_adc_read_async(const struct device *dev, const struct adc_sequence *sequence,
				 struct k_poll_signal *signal)
{
	return read_common(dev, sequence, true, signal);
}
#endif

/* One sampling: each of the sequence's channels, in channel order. */
static void sample(struct costar_adc_data *data)
{
	data->repeat_buffer = data->buffer;
	for (uint32_t channels = data->ctx.sequence.channels; channels != 0; channels &= channels - 1) {
		const uint32_t value = costar_reg_read(data->registers, __builtin_ctz(channels));

		*data->buffer++ = MIN(value, data->full_scale);
	}
}

static void adc_context_start_sampling(struct adc_context *ctx)
{
	struct costar_adc_data *data = CONTAINER_OF(ctx, struct costar_adc_data, ctx);

	/* Without an interval, adc_context_on_sampling_done() starts the next
	 * sampling from inside this one: loop rather than recurse, however many
	 * samplings and repeats the sequence asks for.
	 */
	data->again = true;
	if (data->sampling) {
		return;
	}
	data->sampling = true;
	while (data->again) {
		data->again = false;
		sample(data);
		adc_context_on_sampling_done(ctx, data->dev);
	}
	data->sampling = false;
}

static void adc_context_update_buffer_pointer(struct adc_context *ctx, bool repeat_sampling)
{
	struct costar_adc_data *data = CONTAINER_OF(ctx, struct costar_adc_data, ctx);

	if (repeat_sampling) {
		data->buffer = data->repeat_buffer;
	}
}

static int costar_adc_init(const struct device *dev)
{
	const struct costar_adc_config *config = dev->config;
	struct costar_adc_data *data = dev->data;

	data->dev = dev;
	data->registers = costar_dev(config->costar_device);
	if (data->registers < 0) {
		LOG_ERR("the board has no registers named %s", config->costar_device);
		return -ENODEV;
	}
	adc_context_unlock_unconditionally(&data->ctx);
	return 0;
}

#define COSTAR_ADC_INIT(n)                                                                         \
	BUILD_ASSERT(DT_INST_PROP(n, nchannels) <= 32, "costar,adc has at most 32 channels");      \
	static DEVICE_API(adc, costar_adc_api_##n) = {                                             \
		.channel_setup = costar_adc_channel_setup,                                         \
		.read = costar_adc_read,                                                           \
		IF_ENABLED(CONFIG_ADC_ASYNC, (.read_async = costar_adc_read_async,))               \
		.ref_internal = DT_INST_PROP(n, ref_internal_mv),                                  \
	};                                                                                         \
	static const struct costar_adc_config costar_adc_config_##n = {                            \
		.costar_device = DT_INST_PROP(n, costar_device),                                   \
		.channels = DT_INST_PROP(n, nchannels),                                            \
	};                                                                                         \
	static struct costar_adc_data costar_adc_data_##n = {                                      \
		ADC_CONTEXT_INIT_TIMER(costar_adc_data_##n, ctx),                                  \
		ADC_CONTEXT_INIT_LOCK(costar_adc_data_##n, ctx),                                   \
		ADC_CONTEXT_INIT_SYNC(costar_adc_data_##n, ctx),                                   \
	};                                                                                         \
	DEVICE_DT_INST_DEFINE(n, costar_adc_init, NULL, &costar_adc_data_##n,                      \
			      &costar_adc_config_##n, POST_KERNEL, CONFIG_ADC_INIT_PRIORITY,       \
			      &costar_adc_api_##n);

DT_INST_FOREACH_STATUS_OKAY(COSTAR_ADC_INIT)
