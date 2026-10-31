/*
 * Deterministic entropy.  costar runs must be reproducible, so this is a
 * pseudo-random generator (splitmix64), not a source of real entropy.
 *
 * It is seeded once, at init: the seed property, XORed with registers 0 (low
 * 32 bits) and 1 (high 32 bits) of the registers device named costar-device,
 * if the board has one.  Machines that run the same image draw the same
 * numbers unless the scenario gives them different seeds.
 */
#define DT_DRV_COMPAT costar_entropy

#include <string.h>

#include <zephyr/device.h>
#include <zephyr/drivers/entropy.h>
#include <zephyr/spinlock.h>
#include <zephyr/sys/util.h>

#include "costar.h"

struct costar_entropy_config {
	const char *costar_device;
	uint32_t seed;
};

struct costar_entropy_data {
	struct k_spinlock lock;
	uint64_t state;
};

static uint64_t splitmix64(uint64_t *state)
{
	uint64_t z = (*state += 0x9e3779b97f4a7c15ull);

	z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9ull;
	z = (z ^ (z >> 27)) * 0x94d049bb133111ebull;
	return z ^ (z >> 31);
}

static int costar_entropy_get_entropy(const struct device *dev, uint8_t *buffer, uint16_t length)
{
	struct costar_entropy_data *data = dev->data;
	k_spinlock_key_t key = k_spin_lock(&data->lock);

	for (uint16_t done = 0; done < length;) {
		const uint64_t value = splitmix64(&data->state);
		const uint16_t n = MIN(sizeof(value), length - done);

		memcpy(buffer + done, &value, n);
		done += n;
	}
	k_spin_unlock(&data->lock, key);
	return 0;
}

static int costar_entropy_get_entropy_isr(const struct device *dev, uint8_t *buffer, uint16_t length,
					  uint32_t flags)
{
	ARG_UNUSED(flags);
	costar_entropy_get_entropy(dev, buffer, length);
	return length;
}

static DEVICE_API(entropy, costar_entropy_api) = {
	.get_entropy = costar_entropy_get_entropy,
	.get_entropy_isr = costar_entropy_get_entropy_isr,
};

static int costar_entropy_init(const struct device *dev)
{
	const struct costar_entropy_config *config = dev->config;
	struct costar_entropy_data *data = dev->data;
	const int registers = costar_dev(config->costar_device);

	data->state = config->seed;
	if (registers >= 0) {
		data->state ^= (uint64_t)costar_reg_read(registers, 1) << 32 | costar_reg_read(registers, 0);
	}
	return 0;
}

#define COSTAR_ENTROPY_INIT(n)                                                                     \
	static const struct costar_entropy_config costar_entropy_config_##n = {                    \
		.costar_device = DT_INST_PROP(n, costar_device),                                   \
		.seed = DT_INST_PROP(n, seed),                                                     \
	};                                                                                         \
	static struct costar_entropy_data costar_entropy_data_##n;                                 \
	DEVICE_DT_INST_DEFINE(n, costar_entropy_init, NULL, &costar_entropy_data_##n,              \
			      &costar_entropy_config_##n, PRE_KERNEL_1,                            \
			      CONFIG_ENTROPY_INIT_PRIORITY, &costar_entropy_api);

DT_INST_FOREACH_STATUS_OKAY(COSTAR_ENTROPY_INIT)
