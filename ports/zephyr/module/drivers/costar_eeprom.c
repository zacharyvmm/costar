/*
 * EEPROM on a costar memory device.  The memory's size is the EEPROM's.
 * Unlike flash, any byte can be written at any time: there is no erase, and
 * a write replaces what was there.  Bytes never written read 0xFF.
 * Memory devices keep their contents while the machine is off.
 */
#define DT_DRV_COMPAT costar_eeprom

#include <zephyr/device.h>
#include <zephyr/drivers/eeprom.h>
#include <zephyr/logging/log.h>

#include "costar.h"

LOG_MODULE_REGISTER(costar_eeprom, CONFIG_EEPROM_LOG_LEVEL);

struct costar_eeprom_config {
	const char *costar_device;
	size_t size; /* 0: the memory's */
	bool read_only;
};

struct costar_eeprom_data {
	int memory;
	size_t size;
};

/* True if [offset, offset + length) lies inside the EEPROM. */
static bool in_range(const struct costar_eeprom_data *data, off_t offset, size_t length)
{
	return offset >= 0 && length <= data->size && (uint64_t)offset <= data->size - length;
}

static int costar_eeprom_read(const struct device *dev, off_t offset, void *buffer, size_t length)
{
	struct costar_eeprom_data *data = dev->data;

	if (!in_range(data, offset, length)) {
		return -EINVAL;
	}
	return costar_mem_read(data->memory, (uint32_t)offset, buffer, length) == 0 ? 0 : -EIO;
}

static int costar_eeprom_write(const struct device *dev, off_t offset, const void *buffer, size_t length)
{
	const struct costar_eeprom_config *config = dev->config;
	struct costar_eeprom_data *data = dev->data;

	if (config->read_only) {
		return -EACCES;
	}
	if (!in_range(data, offset, length)) {
		return -EINVAL;
	}
	return costar_mem_write(data->memory, (uint32_t)offset, buffer, length) == 0 ? 0 : -EIO;
}

static size_t costar_eeprom_size(const struct device *dev)
{
	struct costar_eeprom_data *data = dev->data;

	return data->size;
}

static DEVICE_API(eeprom, costar_eeprom_api) = {
	.read = costar_eeprom_read,
	.write = costar_eeprom_write,
	.size = costar_eeprom_size,
};

static int costar_eeprom_init(const struct device *dev)
{
	const struct costar_eeprom_config *config = dev->config;
	struct costar_eeprom_data *data = dev->data;

	data->memory = costar_dev(config->costar_device);
	data->size = costar_mem_size(data->memory);
	if (data->memory < 0 || data->size == 0) {
		LOG_ERR("the board has no memory named %s", config->costar_device);
		return -ENODEV;
	}
	if (config->size != 0 && config->size != data->size) {
		LOG_ERR("%s has %zu bytes, not %zu", config->costar_device, data->size, config->size);
		return -EINVAL;
	}
	return 0;
}

#define COSTAR_EEPROM_INIT(n)                                                                      \
	static const struct costar_eeprom_config costar_eeprom_config_##n = {                      \
		.costar_device = DT_INST_PROP(n, costar_device),                                   \
		.size = DT_INST_PROP_OR(n, size, 0),                                               \
		.read_only = DT_INST_PROP(n, read_only),                                           \
	};                                                                                         \
	static struct costar_eeprom_data costar_eeprom_data_##n;                                   \
	DEVICE_DT_INST_DEFINE(n, costar_eeprom_init, NULL, &costar_eeprom_data_##n,                \
			      &costar_eeprom_config_##n, POST_KERNEL, CONFIG_EEPROM_INIT_PRIORITY, \
			      &costar_eeprom_api);

DT_INST_FOREACH_STATUS_OKAY(COSTAR_EEPROM_INIT)
