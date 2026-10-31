/*
 * Button and LED — an ordinary Zephyr application that uses Zephyr's GPIO API
 * on a costar device (see app.overlay).  Each press of the button (a rising
 * edge on its pin) toggles the LED, which is active low.
 */
#include <zephyr/drivers/gpio.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>

static const struct gpio_dt_spec button = GPIO_DT_SPEC_GET(DT_PATH(zephyr_user), button_gpios);
static const struct gpio_dt_spec led = GPIO_DT_SPEC_GET(DT_PATH(zephyr_user), led_gpios);

static struct gpio_callback pressed_callback;
K_SEM_DEFINE(pressed, 0, 10);

static void on_press(const struct device *port, struct gpio_callback *callback, gpio_port_pins_t pins)
{
	ARG_UNUSED(port);
	ARG_UNUSED(callback);
	ARG_UNUSED(pins);
	k_sem_give(&pressed);
}

int main(void)
{
	if (!gpio_is_ready_dt(&button) || !gpio_is_ready_dt(&led)) {
		printk("gpio not ready\n");
		return 1;
	}
	gpio_pin_configure_dt(&led, GPIO_OUTPUT_INACTIVE);
	gpio_pin_configure_dt(&button, GPIO_INPUT);
	printk("level-triggered: %d\n", gpio_pin_interrupt_configure_dt(&button, GPIO_INT_LEVEL_ACTIVE));
	gpio_pin_interrupt_configure_dt(&button, GPIO_INT_EDGE_TO_ACTIVE);
	gpio_init_callback(&pressed_callback, on_press, BIT(button.pin));
	gpio_add_callback_dt(&button, &pressed_callback);

	for (;;) {
		k_sem_take(&pressed, K_FOREVER);
		gpio_pin_toggle_dt(&led);
		printk("pressed at %u ms: button %d, led %d\n", k_uptime_get_32(), gpio_pin_get_dt(&button),
		       gpio_pin_get_dt(&led));
	}
}
