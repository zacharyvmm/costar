/*
 * Watchdog on a costar watchdog device (COSTAR_WDT_* in costar.h).  One
 * timeout; when it expires, costar reboots the machine.  The timeout is
 * whole milliseconds, with no window (min must be 0), and there is no
 * callback: the reset is the only action.
 */
#define DT_DRV_COMPAT costar_wdt

#include <errno.h>
#include <stdbool.h>

#include <zephyr/device.h>
#include <zephyr/drivers/watchdog.h>

#include "costar.h"

struct costar_wdt_config {
	const char *costar_device;
};

struct costar_wdt_data {
	int dev;             /* the costar device, or -1 if the board has none */
	uint32_t timeout_us;
	bool installed;
	bool running;
};

static int costar_wdt_setup(const struct device *dev, uint8_t options)
{
	struct costar_wdt_data *data = dev->data;

	if (data->dev < 0) {
		return -ENODEV;
	}
	if (data->running) {
		return -EBUSY;
	}
	if ((options & WDT_OPT_PAUSE_IN_SLEEP) != 0) {
		return -ENOTSUP; /* it counts while the CPU waits, as it would on a target */
	}
	if (!data->installed) {
		return -EINVAL;
	}
	costar_reg_write(data->dev, COSTAR_WDT_TIMEOUT, data->timeout_us);
	costar_reg_write(data->dev, COSTAR_WDT_CTRL, 1);
	data->running = true;
	return 0;
}

static int costar_wdt_disable(const struct device *dev)
{
	struct costar_wdt_data *data = dev->data;

	if (!data->running) {
		return -EFAULT;
	}
	costar_reg_write(data->dev, COSTAR_WDT_CTRL, 0);
	data->running = false;
	data->installed = false;
	return 0;
}

static int costar_wdt_install_timeout(const struct device *dev, const struct wdt_timeout_cfg *cfg)
{
	struct costar_wdt_data *data = dev->data;

	if (data->dev < 0) {
		return -ENODEV;
	}
	if (data->running) {
		return -EBUSY;
	}
	if (data->installed) {
		return -ENOMEM;
	}
	if (cfg->callback != NULL || (cfg->flags & WDT_FLAG_RESET_MASK) == WDT_FLAG_RESET_NONE) {
		return -ENOTSUP;
	}
	if (cfg->window.min != 0 || cfg->window.max == 0 || cfg->window.max > UINT32_MAX / 1000) {
		return -EINVAL;
	}
	data->timeout_us = cfg->window.max * 1000;
	data->installed = true;
	return 0; /* the only channel */
}

static int costar_wdt_feed(const struct device *dev, int channel_id)
{
	struct costar_wdt_data *data = dev->data;

	if (channel_id != 0 || !data->installed) {
		return -EINVAL;
	}
	costar_reg_write(data->dev, COSTAR_WDT_FEED, 1);
	return 0;
}

static DEVICE_API(wdt, costar_wdt_api) = {
	.setup = costar_wdt_setup,
	.disable = costar_wdt_disable,
	.install_timeout = costar_wdt_install_timeout,
	.feed = costar_wdt_feed,
};

static int costar_wdt_init(const struct device *dev)
{
	const struct costar_wdt_config *config = dev->config;
	struct costar_wdt_data *data = dev->data;

	data->dev = costar_dev(config->costar_device);
	return 0;
}

#define COSTAR_WDT_INIT(n)                                                                         \
	static const struct costar_wdt_config costar_wdt_config_##n = {                            \
		.costar_device = DT_INST_PROP(n, costar_device),                                   \
	};                                                                                         \
	static struct costar_wdt_data costar_wdt_data_##n;                                         \
	DEVICE_DT_INST_DEFINE(n, costar_wdt_init, NULL, &costar_wdt_data_##n,                      \
			      &costar_wdt_config_##n, PRE_KERNEL_1,                                \
			      CONFIG_KERNEL_INIT_PRIORITY_DEVICE, &costar_wdt_api);

DT_INST_FOREACH_STATUS_OKAY(COSTAR_WDT_INIT)
