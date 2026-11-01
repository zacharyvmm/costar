/*
 * An ordinary Zephyr application with a SPI NOR flash (see app.overlay),
 * driven by Zephyr's own jedec,spi-nor driver on costar's SPI controller.
 * On the host, the scenario's spi-nor model answers for the flash, whose
 * contents are a memory device of a passive board.
 *
 * Built as it is, the app checks the flash API: an erase, a write across a
 * page boundary read back (in frames of 64 bytes:
 * CONFIG_COSTAR_SPI_FRAME_SIZE), a write over written bits (NOR flash only
 * clears bits) and a 64 KiB block erase.  It also reads the JEDEC ID itself,
 * with the SPI API, in a transaction that spans two transceives (the chip
 * select held between them).
 *
 * Built with CONFIG_NVS, it counts its boots in NVS on the flash's first
 * two sectors instead: the flash outlives the MCU's reboots.
 */
#include <string.h>

#include <zephyr/device.h>
#include <zephyr/drivers/flash.h>
#include <zephyr/drivers/spi.h>
#include <zephyr/kernel.h>
#include <zephyr/kvss/nvs.h>
#include <zephyr/storage/flash_map.h>
#include <zephyr/sys/printk.h>

#define SECTOR 4096
#define CHECKED 0x10000
#define BLOCK 0x20000

static const struct device *const flash = DEVICE_DT_GET(DT_NODELABEL(flash));

#ifdef CONFIG_NVS

static void count_boot(void)
{
	struct nvs_fs fs = {
		.flash_device = flash,
		.offset = PARTITION_OFFSET(nvs_partition),
		.sector_size = SECTOR,
		.sector_count = PARTITION_SIZE(nvs_partition) / SECTOR,
	};
	uint32_t boots = 0;
	int ret = nvs_mount(&fs);

	if (ret != 0) {
		printk("nvs_mount: %d\n", ret);
		return;
	}
	if (nvs_read(&fs, 1, &boots, sizeof(boots)) != sizeof(boots)) {
		boots = 0;
	}
	boots++;
	ret = nvs_write(&fs, 1, &boots, sizeof(boots));
	printk("boot %u (nvs_write %d)\n", boots, ret);
}

#else

static const struct spi_dt_spec flash_spi =
	SPI_DT_SPEC_GET(DT_NODELABEL(flash), SPI_WORD_SET(8) | SPI_HOLD_ON_CS | SPI_LOCK_ON);

static void read_id(void)
{
	uint8_t command = 0x9f; /* read JEDEC ID */
	uint8_t id[3] = {0};
	const struct spi_buf tx = {.buf = &command, .len = 1};
	const struct spi_buf rx = {.buf = id, .len = sizeof(id)};
	const struct spi_buf_set tx_set = {.buffers = &tx, .count = 1};
	const struct spi_buf_set rx_set = {.buffers = &rx, .count = 1};
	int ret = spi_write_dt(&flash_spi, &tx_set);

	if (ret == 0) {
		ret = spi_read_dt(&flash_spi, &rx_set);
	}
	spi_release_dt(&flash_spi);
	printk("JEDEC ID by hand %02x %02x %02x (%d)\n", id[0], id[1], id[2], ret);
}

static void check_flash(void)
{
	uint8_t data[100];
	uint8_t back[sizeof(data)];
	uint8_t byte;
	uint32_t start;
	int ret;

	for (size_t i = 0; i < sizeof(data); i++) {
		data[i] = (uint8_t)(i * 7);
	}
	start = k_uptime_get_32();
	ret = flash_erase(flash, CHECKED, SECTOR);
	printk("erased a sector (%d) in %u ms\n", ret, k_uptime_get_32() - start);
	ret = flash_write(flash, CHECKED + 200, data, sizeof(data));
	memset(back, 0, sizeof(back));
	flash_read(flash, CHECKED + 200, back, sizeof(back));
	printk("wrote %zu bytes across a page (%d): %s\n", sizeof(data), ret,
	       memcmp(data, back, sizeof(data)) == 0 ? "read back" : "different");

	byte = 0xf0;
	flash_write(flash, CHECKED, &byte, 1);
	byte = 0x0f;
	flash_write(flash, CHECKED, &byte, 1);
	flash_read(flash, CHECKED, &byte, 1);
	printk("0xf0, then 0x0f over it: 0x%02x\n", byte);

	start = k_uptime_get_32();
	ret = flash_erase(flash, BLOCK, 0x10000);
	flash_read(flash, BLOCK + 0x8000, &byte, 1);
	printk("erased a block (%d) in %u ms: 0x%02x\n", ret, k_uptime_get_32() - start, byte);
}

#endif

int main(void)
{
	uint8_t id[3] = {0};

	flash_read_jedec_id(flash, id);
	printk("flash ready %d, JEDEC ID %02x %02x %02x\n", device_is_ready(flash), id[0], id[1], id[2]);
#ifdef CONFIG_NVS
	count_boot();
#else
	read_id();
	check_flash();
#endif
	return 0;
}
