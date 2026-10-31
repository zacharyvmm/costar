/*
 * Random numbers — an ordinary Zephyr application that draws from the
 * zephyr,entropy device and from sys_rand_get().  On costar both come from
 * costar,entropy, so they are the same every run, and differ between
 * machines given different seeds.
 */
#include <zephyr/device.h>
#include <zephyr/drivers/entropy.h>
#include <zephyr/kernel.h>
#include <zephyr/random/random.h>
#include <zephyr/sys/printk.h>

static const struct device *const entropy = DEVICE_DT_GET(DT_CHOSEN(zephyr_entropy));

int main(void)
{
	uint8_t bytes[6];

	if (!device_is_ready(entropy)) {
		printk("entropy not ready\n");
		return 1;
	}
	entropy_get_entropy(entropy, bytes, sizeof(bytes));
	printk("entropy %02x %02x %02x %02x %02x %02x\n", bytes[0], bytes[1], bytes[2], bytes[3],
	       bytes[4], bytes[5]);
	for (int i = 0; i < 3; i++) {
		printk("rand %08x\n", sys_rand32_get());
	}
	return 0;
}
