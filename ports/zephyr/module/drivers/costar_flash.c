/*
 * Flash on a costar memory device.  The memory device's size is the flash
 * size; erased bytes read 0xFF.
 */
#define DT_DRV_COMPAT costar_flash

#include <zephyr/device.h>
#include <zephyr/drivers/flash.h>
#include <zephyr/logging/log.h>

#include "costar.h"

LOG_MODULE_REGISTER(costar_flash, CONFIG_FLASH_LOG_LEVEL);

struct costar_flash_config {
	const char *costar_device;
	struct flash_parameters parameters;
	size_t erase_block_size;
};

struct costar_flash_data {
	int memory;
	size_t size;
	struct flash_pages_layout layout;
};

static int costar_flash_read(const struct device *dev, off_t offset, void *buffer, size_t length)
{
	struct costar_flash_data *data = dev->data;

	return costar_mem_read(data->memory, (uint32_t)offset, buffer, length) == 0 ? 0 : -EINVAL;
}

static int costar_flash_write(const struct device *dev, off_t offset, const void *buffer, size_t length)
{
	const struct costar_flash_config *config = dev->config;
	struct costar_flash_data *data = dev->data;

	if (offset % config->parameters.write_block_size != 0 ||
	    length % config->parameters.write_block_size != 0) {
		return -EINVAL;
	}
	return costar_mem_write(data->memory, (uint32_t)offset, buffer, length) == 0 ? 0 : -EINVAL;
}

static int costar_flash_erase(const struct device *dev, off_t offset, size_t length)
{
	const struct costar_flash_config *config = dev->config;
	struct costar_flash_data *data = dev->data;

	if (offset % config->erase_block_size != 0 || length % config->erase_block_size != 0) {
		return -EINVAL;
	}
	return costar_mem_erase(data->memory, (uint32_t)offset, length) == 0 ? 0 : -EINVAL;
}

static const struct flash_parameters *costar_flash_get_parameters(const struct device *dev)
{
	const struct costar_flash_config *config = dev->config;

	return &config->parameters;
}

static int costar_flash_get_size(const struct device *dev, uint64_t *size)
{
	struct costar_flash_data *data = dev->data;

	*size = data->size;
	return 0;
}

static void costar_flash_page_layout(const struct device *dev, const struct flash_pages_layout **layout,
				     size_t *layout_size)
{
	struct costar_flash_data *data = dev->data;

	*layout = &data->layout;
	*layout_size = 1;
}

static DEVICE_API(flash, costar_flash_api) = {
	.read = costar_flash_read,
	.write = costar_flash_write,
	.erase = costar_flash_erase,
	.get_parameters = costar_flash_get_parameters,
	.get_size = costar_flash_get_size,
#ifdef CONFIG_FLASH_PAGE_LAYOUT
	.page_layout = costar_flash_page_layout,
#endif
};

static int costar_flash_init(const struct device *dev)
{
	const struct costar_flash_config *config = dev->config;
	struct costar_flash_data *data = dev->data;

	data->memory = costar_dev(config->costar_device);
	data->size = costar_mem_size(data->memory);
	if (data->memory < 0 || data->size == 0) {
		LOG_ERR("the board has no memory named %s", config->costar_device);
		return -ENODEV;
	}
	if (data->size % config->erase_block_size != 0) {
		LOG_ERR("%s is not a whole number of %zu-byte blocks", config->costar_device,
			config->erase_block_size);
		return -EINVAL;
	}
	data->layout.pages_count = data->size / config->erase_block_size;
	data->layout.pages_size = config->erase_block_size;
	return 0;
}

#define COSTAR_FLASH_INIT(n)                                                                       \
	static const struct costar_flash_config costar_flash_config_##n = {                        \
		.costar_device = DT_INST_PROP(n, costar_device),                                   \
		.parameters = {.write_block_size = DT_INST_PROP(n, write_block_size),              \
			       .erase_value = 0xff},                                               \
		.erase_block_size = DT_INST_PROP(n, erase_block_size),                             \
	};                                                                                         \
	static struct costar_flash_data costar_flash_data_##n;                                     \
	DEVICE_DT_INST_DEFINE(n, costar_flash_init, NULL, &costar_flash_data_##n,                  \
			      &costar_flash_config_##n, POST_KERNEL, CONFIG_FLASH_INIT_PRIORITY,   \
			      &costar_flash_api);

DT_INST_FOREACH_STATUS_OKAY(COSTAR_FLASH_INIT)
