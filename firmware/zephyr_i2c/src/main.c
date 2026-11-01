/*
 * A temperature logger: an ordinary Zephyr application whose parts sit on
 * an I2C bus (see app.overlay), driven by Zephyr's own ti,tmp112 sensor and
 * atmel,at24 EEPROM drivers on costar's I2C controller.  On the host, the
 * scenario's device models answer for the parts.
 *
 * At boot the app reads a greeting from the EEPROM and writes a line that
 * spans three of its pages (the driver waits out each page's write cycle by
 * polling: the EEPROM NACKs while it is busy).  Then it reads the
 * temperature every 100 ms and logs each reading in the EEPROM, and reads
 * the log back.
 *
 * Finally it reads the EEPROM through the I2C API itself, twice, while the
 * scenario delays its requests on the wire past the bus's transfer timeout
 * (10 ms, in app.overlay): the first by 15 ms, the second by 8.  The first
 * read times out, and its late reply arrives while the second waits, which
 * must not take it for its own.
 */
#include <stdlib.h>
#include <string.h>

#include <zephyr/device.h>
#include <zephyr/drivers/eeprom.h>
#include <zephyr/drivers/i2c.h>
#include <zephyr/drivers/sensor.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/byteorder.h>
#include <zephyr/sys/printk.h>

#define READINGS 6
#define LOG_OFFSET 0x80

static const struct device *const temperature = DEVICE_DT_GET(DT_NODELABEL(temperature));
static const struct device *const missing = DEVICE_DT_GET(DT_NODELABEL(missing));
static const struct device *const eeprom = DEVICE_DT_GET(DT_NODELABEL(eeprom));
static const struct i2c_dt_spec eeprom_i2c = I2C_DT_SPEC_GET(DT_NODELABEL(eeprom));

static void greet(void)
{
	static const char line[] = "written by zephyr!!";
	char text[sizeof(line)] = {0};
	int ret;

	ret = eeprom_read(eeprom, 0, text, 5);
	printk("eeprom says \"%s\" (%d)\n", text, ret);

	ret = eeprom_write(eeprom, 4, line, sizeof(line));
	printk("wrote %zu bytes at 4 (%d) by %u ms\n", sizeof(line), ret, k_uptime_get_32());
	memset(text, 0, sizeof(text));
	ret = eeprom_read(eeprom, 4, text, sizeof(text));
	printk("read back \"%s\" (%d)\n", text, ret);
}

int main(void)
{
	printk("tmp112@48 ready %d, tmp112@49 ready %d, eeprom ready %d (%zu bytes)\n",
	       device_is_ready(temperature), device_is_ready(missing), device_is_ready(eeprom),
	       eeprom_get_size(eeprom));
	greet();

	for (int i = 0; i < READINGS; i++) {
		struct sensor_value value;
		int ret;

		k_sleep(K_TIMEOUT_ABS_MS(100 * (i + 1)));
		ret = sensor_sample_fetch(temperature);
		if (ret != 0) {
			printk("%u ms: fetch failed (%d)\n", k_uptime_get_32(), ret);
			continue;
		}
		sensor_channel_get(temperature, SENSOR_CHAN_AMBIENT_TEMP, &value);

		const int32_t milli = (int32_t)sensor_value_to_milli(&value);
		uint8_t bytes[2];

		printk("%u ms: %d.%03d C\n", k_uptime_get_32(), milli / 1000, abs(milli % 1000));
		sys_put_be16((uint16_t)(milli / 100), bytes); /* tenths of a degree */
		eeprom_write(eeprom, LOG_OFFSET + 2 * i, bytes, sizeof(bytes));
	}

	uint8_t log[2 * READINGS];

	eeprom_read(eeprom, LOG_OFFSET, log, sizeof(log));
	printk("log:");
	for (int i = 0; i < READINGS; i++) {
		printk(" %d", (int16_t)sys_get_be16(&log[2 * i]));
	}
	printk("\n");

	k_sleep(K_TIMEOUT_ABS_MS(700));
	for (uint8_t offset = 0; offset <= 4; offset += 4) {
		char text[5] = {0};
		const int ret = i2c_write_read_dt(&eeprom_i2c, &offset, 1, text, 4);

		printk("%u ms: read \"%s\" at %u (%d)\n", k_uptime_get_32(), text, offset, ret);
	}
	return 0;
}
