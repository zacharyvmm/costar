/*
 * Button and LED — an ordinary Zephyr application that uses Zephyr's GPIO API
 * on a costar device (see app.overlay).  Each press of the button (a rising
 * edge on its pin) toggles the LED, which is active low.
 *
 * Meanwhile main() puts a third pin, the probe, through interrupt
 * configuration changes while the scenario toggles it, and logs each edge
 * the probe's callback sees.
 */
#include <zephyr/drivers/gpio.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>

static const struct gpio_dt_spec button = GPIO_DT_SPEC_GET(DT_PATH(zephyr_user), button_gpios);
static const struct gpio_dt_spec led = GPIO_DT_SPEC_GET(DT_PATH(zephyr_user), led_gpios);
static const struct gpio_dt_spec probe = GPIO_DT_SPEC_GET(DT_PATH(zephyr_user), probe_gpios);

static struct gpio_callback pressed_callback;
static struct gpio_callback probe_callback;
K_SEM_DEFINE(pressed, 0, 10);

static void on_press(const struct device *port, struct gpio_callback *callback, gpio_port_pins_t pins)
{
	ARG_UNUSED(port);
	ARG_UNUSED(callback);
	ARG_UNUSED(pins);
	k_sem_give(&pressed);
}

static void on_probe(const struct device *port, struct gpio_callback *callback, gpio_port_pins_t pins)
{
	ARG_UNUSED(port);
	ARG_UNUSED(callback);
	ARG_UNUSED(pins);
	printk("probe edge at %u ms\n", k_uptime_get_32());
}

static void buttons(void *a, void *b, void *c)
{
	ARG_UNUSED(a);
	ARG_UNUSED(b);
	ARG_UNUSED(c);
	for (;;) {
		k_sem_take(&pressed, K_FOREVER);
		gpio_pin_toggle_dt(&led);
		printk("pressed at %u ms: button %d, led %d\n", k_uptime_get_32(), gpio_pin_get_dt(&button),
		       gpio_pin_get_dt(&led));
	}
}

K_THREAD_DEFINE(buttons_thread, 1024, buttons, NULL, NULL, NULL, 5, 0, 0);

static void at_ms(uint32_t ms, const char *what)
{
	k_sleep(K_TIMEOUT_ABS_MS(ms));
	printk("%s\n", what);
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

	gpio_pin_configure_dt(&probe, GPIO_INPUT);
	gpio_init_callback(&probe_callback, on_probe, BIT(probe.pin));
	gpio_add_callback_dt(&probe, &probe_callback);
	/* The probe rises at 6.5 ms, with interrupts locked and its own disabled.
	 * Enabling it then doesn't report that earlier edge.
	 */
	k_sleep(K_TIMEOUT_ABS_MS(6));
	unsigned int key = irq_lock();

	k_busy_wait(1000);
	gpio_pin_interrupt_configure_dt(&probe, GPIO_INT_EDGE_RISING);
	irq_unlock(key);
	printk("probe interrupt on at %u us\n", k_cyc_to_us_floor32(k_cycle_get_32()));
	/* Rises again at 9 ms: seen. */
	at_ms(10, "probe interrupt disabled only");
	gpio_pin_interrupt_configure_dt(&probe, GPIO_INT_MODE_DISABLE_ONLY);
	/* Rises at 12 ms: not seen. */
	at_ms(13, "probe interrupt enabled only");
	gpio_pin_interrupt_configure_dt(&probe, GPIO_INT_MODE_ENABLE_ONLY);
	/* Rises at 15 ms: seen, still for rising edges only. */
	at_ms(16, "probe disconnected");
	gpio_pin_configure_dt(&probe, GPIO_DISCONNECTED);
	/* Rises at 18 ms: not seen, and reads 0. */
	k_sleep(K_TIMEOUT_ABS_MS(19));
	printk("probe reads %d\n", gpio_pin_get_dt(&probe));
	return 0;
}
