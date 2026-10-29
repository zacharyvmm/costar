/*
 * UART echo with a boot counter — an ordinary Zephyr application that uses
 * Zephyr's UART and flash APIs on costar devices (see app.overlay).
 *
 * At boot it increments a counter in flash.  Then the UART's interrupt
 * handler queues every received byte, and main() echoes each one back in
 * upper case.
 */
#include <ctype.h>

#include <zephyr/drivers/flash.h>
#include <zephyr/drivers/uart.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>

static const struct device *const uart = DEVICE_DT_GET(DT_NODELABEL(costar_uart));
static const struct device *const flash = DEVICE_DT_GET(DT_NODELABEL(costar_flash));

K_MSGQ_DEFINE(received, sizeof(uint8_t), 16, 1);

static void uart_isr(const struct device *dev, void *user_data)
{
	uint8_t byte;

	ARG_UNUSED(user_data);
	uart_irq_update(dev);
	while (uart_irq_rx_ready(dev) && uart_fifo_read(dev, &byte, 1) == 1) {
		k_msgq_put(&received, &byte, K_NO_WAIT);
	}
}

static uint32_t count_boot(void)
{
	uint32_t boots;

	flash_read(flash, 0, &boots, sizeof(boots));
	boots = boots == UINT32_MAX ? 1 : boots + 1;
	flash_erase(flash, 0, 256);
	flash_write(flash, 0, &boots, sizeof(boots));
	return boots;
}

int main(void)
{
	if (!device_is_ready(uart) || !device_is_ready(flash)) {
		printk("devices not ready\n");
		return 1;
	}
	printk("boot %u\n", count_boot());

	uart_irq_callback_set(uart, uart_isr);
	uart_irq_rx_enable(uart);

	for (;;) {
		uint8_t byte;

		k_msgq_get(&received, &byte, K_FOREVER);
		printk("got %c at %llu us\n", byte, k_cyc_to_us_floor64(k_cycle_get_64()));
		uart_poll_out(uart, (unsigned char)toupper(byte));
	}
}
