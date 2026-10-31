/*
 * PWM on a costar registers device.  Channel c has three registers:
 *
 *   register 3c      the period, in cycles
 *   register 3c + 1  the pulse (the active time), in cycles
 *   register 3c + 2  the flags: 1 for inverted polarity (active low)
 *
 * The driver writes them whenever they change, so the trace shows the
 * output as `reg` records and plants read it from the registers: the duty
 * cycle is pulse / period.  A period of 0 is an output that was never set.
 * The clock runs at clock-frequency cycles per second (default 1 GHz, so
 * cycles are nanoseconds).
 *
 * Registers keep their values while a machine is off, as all costar
 * registers do; at boot the driver sets every channel back to 0, as a reset
 * PWM peripheral stops its outputs.  Capture isn't supported.
 */
#define DT_DRV_COMPAT costar_pwm

#include <zephyr/device.h>
#include <zephyr/drivers/pwm.h>
#include <zephyr/logging/log.h>
#include <zephyr/spinlock.h>

#include "costar.h"

LOG_MODULE_REGISTER(costar_pwm, CONFIG_PWM_LOG_LEVEL);

enum { REG_PERIOD, REG_PULSE, REG_FLAGS, REGS_PER_CHANNEL };

struct costar_pwm_config {
	const char *costar_device;
	uint32_t channels;
	uint32_t cycles_per_sec;
};

struct costar_pwm_data {
	struct k_spinlock lock;
	int registers;
	uint32_t (*outputs)[REGS_PER_CHANNEL]; /* what the registers say, per channel */
};

/* Set a channel's registers that differ from `values`.  Under the lock. */
static void drive(struct costar_pwm_data *data, uint32_t channel, const uint32_t values[REGS_PER_CHANNEL])
{
	for (uint32_t i = 0; i < REGS_PER_CHANNEL; i++) {
		if (data->outputs[channel][i] != values[i]) {
			data->outputs[channel][i] = values[i];
			costar_reg_write(data->registers, channel * REGS_PER_CHANNEL + i, values[i]);
		}
	}
}

static int costar_pwm_set_cycles(const struct device *dev, uint32_t channel, uint32_t period_cycles,
				 uint32_t pulse_cycles, pwm_flags_t flags)
{
	const struct costar_pwm_config *config = dev->config;
	struct costar_pwm_data *data = dev->data;
	const uint32_t values[REGS_PER_CHANNEL] = {period_cycles, pulse_cycles, flags};

	if (channel >= config->channels) {
		return -EINVAL;
	}
	if ((flags & ~PWM_POLARITY_INVERTED) != 0) {
		return -ENOTSUP;
	}

	k_spinlock_key_t key = k_spin_lock(&data->lock);

	drive(data, channel, values);
	k_spin_unlock(&data->lock, key);
	return 0;
}

static int costar_pwm_get_cycles_per_sec(const struct device *dev, uint32_t channel, uint64_t *cycles)
{
	const struct costar_pwm_config *config = dev->config;

	if (channel >= config->channels) {
		return -EINVAL;
	}
	*cycles = config->cycles_per_sec;
	return 0;
}

static DEVICE_API(pwm, costar_pwm_api) = {
	.set_cycles = costar_pwm_set_cycles,
	.get_cycles_per_sec = costar_pwm_get_cycles_per_sec,
};

static int costar_pwm_init(const struct device *dev)
{
	const struct costar_pwm_config *config = dev->config;
	struct costar_pwm_data *data = dev->data;
	static const uint32_t off[REGS_PER_CHANNEL];

	data->registers = costar_dev(config->costar_device);
	if (data->registers < 0) {
		LOG_ERR("the board has no registers named %s", config->costar_device);
		return -ENODEV;
	}
	for (uint32_t channel = 0; channel < config->channels; channel++) {
		for (uint32_t i = 0; i < REGS_PER_CHANNEL; i++) {
			data->outputs[channel][i] = costar_reg_read(data->registers, channel * REGS_PER_CHANNEL + i);
		}
		drive(data, channel, off);
	}
	return 0;
}

#define COSTAR_PWM_INIT(n)                                                                         \
	static uint32_t costar_pwm_outputs_##n[DT_INST_PROP(n, channels)][REGS_PER_CHANNEL];       \
	static const struct costar_pwm_config costar_pwm_config_##n = {                            \
		.costar_device = DT_INST_PROP(n, costar_device),                                   \
		.channels = DT_INST_PROP(n, channels),                                             \
		.cycles_per_sec = DT_INST_PROP(n, clock_frequency),                                \
	};                                                                                         \
	static struct costar_pwm_data costar_pwm_data_##n = {                                      \
		.outputs = costar_pwm_outputs_##n,                                                 \
	};                                                                                         \
	DEVICE_DT_INST_DEFINE(n, costar_pwm_init, NULL, &costar_pwm_data_##n,                      \
			      &costar_pwm_config_##n, POST_KERNEL, CONFIG_PWM_INIT_PRIORITY,       \
			      &costar_pwm_api);

DT_INST_FOREACH_STATUS_OKAY(COSTAR_PWM_INIT)
