/*
 * Watchdog — main() starts the costar,wdt watchdog (the shield's watchdog0)
 * with a 100 ms timeout, feeds it three times, 50 ms apart, then stops, as if
 * it hung.  100 ms after the last feed the watchdog reboots the machine, and
 * it all starts again.  It also prints what the driver returns for what it
 * doesn't support, and for disabling and starting again.
 */
#include <zephyr/drivers/watchdog.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>

int main(void)
{
	const struct device *wdt = DEVICE_DT_GET(DT_ALIAS(watchdog0));
	struct wdt_timeout_cfg config = {
		.window = {.min = 0, .max = 100},
		.flags = WDT_FLAG_RESET_SOC,
	};
	struct wdt_timeout_cfg windowed = config;

	windowed.window.min = 10;
	printk("boot\n");
	printk("disable before setup: %d\n", wdt_disable(wdt));
	printk("windowed: %d\n", wdt_install_timeout(wdt, &windowed));

	int channel = wdt_install_timeout(wdt, &config);

	printk("channel %d, another: %d\n", channel, wdt_install_timeout(wdt, &config));
	printk("pause in sleep: %d\n", wdt_setup(wdt, WDT_OPT_PAUSE_IN_SLEEP));
	printk("setup: %d\n", wdt_setup(wdt, WDT_OPT_PAUSE_HALTED_BY_DBG));
	printk("disable: %d\n", wdt_disable(wdt));
	channel = wdt_install_timeout(wdt, &config);
	printk("setup again: %d\n", wdt_setup(wdt, 0));
	for (int i = 0; i < 3; i++) {
		k_msleep(50);
		printk("feed: %d\n", wdt_feed(wdt, channel));
	}
	printk("hanging\n");
	k_sleep(K_FOREVER);
	return 0;
}
