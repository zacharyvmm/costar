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

static void print_bytes(const char *label, int result, const uint8_t *bytes, size_t length)
{
	printk("%s %d", label, result);
	for (size_t i = 0; i < length; i++) {
		printk(" %02x", bytes[i]);
	}
	printk("\n");
}

/* From an interrupt: a timer's expiry function. */
static uint8_t isr_bytes[2][5];
static int isr_results[2];
static K_SEM_DEFINE(isr_done, 0, 1);

static void draw_in_isr(struct k_timer *timer)
{
	ARG_UNUSED(timer);
	isr_results[0] = entropy_get_entropy_isr(entropy, isr_bytes[0], sizeof(isr_bytes[0]), 0);
	isr_results[1] = entropy_get_entropy_isr(entropy, isr_bytes[1], sizeof(isr_bytes[1]),
						 ENTROPY_BUSYWAIT);
	k_sem_give(&isr_done);
}

static K_TIMER_DEFINE(isr_timer, draw_in_isr, NULL);

int main(void)
{
	uint8_t bytes[6];
	uint8_t blocks[20]; /* two whole 8-byte draws and part of a third */
	int result;

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
	result = entropy_get_entropy(entropy, blocks, sizeof(blocks));
	print_bytes("blocks", result, blocks, sizeof(blocks));
	k_timer_start(&isr_timer, K_MSEC(1), K_NO_WAIT);
	k_sem_take(&isr_done, K_FOREVER);
	print_bytes("isr", isr_results[0], isr_bytes[0], sizeof(isr_bytes[0]));
	print_bytes("isr busywait", isr_results[1], isr_bytes[1], sizeof(isr_bytes[1]));
	return 0;
}
