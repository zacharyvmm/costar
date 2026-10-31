/*
 * A GPIO port on a costar registers device.
 *
 *   register 0  input levels, one bit per pin, set by the host or a plant
 *   register 1  output levels: the levels of the pins configured as outputs,
 *               written by this driver whenever they change
 *
 * A pin configured as an output reads its own output level, an input pin
 * reads register 0, and a disconnected pin reads 0.  The board raises the
 * device's IRQ when the host changes a register, and the driver then runs
 * the callbacks of input pins whose edge-triggered interrupts match the
 * change.  Level-triggered interrupts, pull resistors and open-drain outputs
 * aren't supported.
 *
 * Edges are found by comparing register 0 with its value when the ISR last
 * ran; the device keeps no history.  A pulse that begins and ends while
 * interrupts are locked (across virtual time: a busy wait in a critical
 * section) is not seen, unlike on hardware that latches edges.
 */
#define DT_DRV_COMPAT costar_gpio

#include <zephyr/device.h>
#include <zephyr/drivers/gpio.h>
#include <zephyr/drivers/gpio/gpio_utils.h>
#include <zephyr/irq.h>
#include <zephyr/logging/log.h>
#include <zephyr/spinlock.h>

#include "costar.h"

LOG_MODULE_REGISTER(costar_gpio, CONFIG_GPIO_LOG_LEVEL);

#define REG_INPUTS  0
#define REG_OUTPUTS 1

struct costar_gpio_config {
	struct gpio_driver_config common; /* first: Zephyr's GPIO core expects it */
	const char *costar_device;
	int irq;                          /* -1: none */
	void (*irq_config)(void);
};

struct costar_gpio_data {
	struct gpio_driver_data common; /* first: Zephyr's GPIO core expects it */
	struct k_spinlock lock;
	int registers;
	gpio_port_value_t latch;        /* output levels, driven on output pins */
	gpio_port_pins_t outputs;       /* pins configured as outputs */
	gpio_port_pins_t inputs;        /* pins configured as inputs */
	gpio_port_value_t driven;       /* what register 1 last said */
	gpio_port_value_t levels;       /* register 0 when the ISR last looked */
	gpio_port_pins_t rising;        /* pins whose interrupt is for rising edges */
	gpio_port_pins_t falling;       /* ... and for falling edges */
	gpio_port_pins_t enabled;       /* pins whose interrupt is enabled */
	sys_slist_t callbacks;
};

/* Tell the board the output levels, if they changed.  Under the lock. */
static void drive(struct costar_gpio_data *data)
{
	const gpio_port_value_t driven = data->latch & data->outputs;

	if (driven != data->driven) {
		data->driven = driven;
		costar_reg_write(data->registers, REG_OUTPUTS, driven);
	}
}

static int costar_gpio_pin_configure(const struct device *dev, gpio_pin_t pin, gpio_flags_t flags)
{
	struct costar_gpio_data *data = dev->data;
	const gpio_port_pins_t bit = BIT(pin);

	if ((flags & (GPIO_SINGLE_ENDED | GPIO_PULL_UP | GPIO_PULL_DOWN)) != 0) {
		return -ENOTSUP;
	}
	if ((flags & GPIO_INPUT) != 0 && (flags & GPIO_OUTPUT) != 0) {
		return -ENOTSUP;
	}

	k_spinlock_key_t key = k_spin_lock(&data->lock);

	if ((flags & GPIO_OUTPUT_INIT_HIGH) != 0) {
		data->latch |= bit;
	} else if ((flags & GPIO_OUTPUT_INIT_LOW) != 0) {
		data->latch &= ~bit;
	}
	data->outputs = (flags & GPIO_OUTPUT) != 0 ? data->outputs | bit : data->outputs & ~bit;
	data->inputs = (flags & GPIO_INPUT) != 0 ? data->inputs | bit : data->inputs & ~bit;
	drive(data);
	k_spin_unlock(&data->lock, key);
	return 0;
}

static int costar_gpio_port_get_raw(const struct device *dev, gpio_port_value_t *value)
{
	struct costar_gpio_data *data = dev->data;
	k_spinlock_key_t key = k_spin_lock(&data->lock);
	const gpio_port_value_t levels = costar_reg_read(data->registers, REG_INPUTS);

	*value = (levels & data->inputs) | (data->latch & data->outputs);
	k_spin_unlock(&data->lock, key);
	return 0;
}

/* latch = (latch & ~clear) ^ toggle | set, then drive. */
static int update(const struct device *dev, gpio_port_pins_t clear, gpio_port_value_t set,
		  gpio_port_pins_t toggle)
{
	struct costar_gpio_data *data = dev->data;
	k_spinlock_key_t key = k_spin_lock(&data->lock);

	data->latch = ((data->latch & ~clear) ^ toggle) | set;
	drive(data);
	k_spin_unlock(&data->lock, key);
	return 0;
}

static int costar_gpio_port_set_masked_raw(const struct device *dev, gpio_port_pins_t mask,
					   gpio_port_value_t value)
{
	return update(dev, mask, value & mask, 0);
}

static int costar_gpio_port_set_bits_raw(const struct device *dev, gpio_port_pins_t pins)
{
	return update(dev, 0, pins, 0);
}

static int costar_gpio_port_clear_bits_raw(const struct device *dev, gpio_port_pins_t pins)
{
	return update(dev, pins, 0, 0);
}

static int costar_gpio_port_toggle_bits(const struct device *dev, gpio_port_pins_t pins)
{
	return update(dev, 0, 0, pins);
}

static int costar_gpio_pin_interrupt_configure(const struct device *dev, gpio_pin_t pin,
					       enum gpio_int_mode mode, enum gpio_int_trig trig)
{
	const struct costar_gpio_config *config = dev->config;
	struct costar_gpio_data *data = dev->data;
	const gpio_port_pins_t bit = BIT(pin);

	if (mode == GPIO_INT_MODE_LEVEL || (mode != GPIO_INT_MODE_DISABLED && config->irq < 0)) {
		return -ENOTSUP;
	}

	k_spinlock_key_t key = k_spin_lock(&data->lock);

	switch ((int)mode) {
	case GPIO_INT_MODE_DISABLED:
		data->rising &= ~bit;
		data->falling &= ~bit;
		data->enabled &= ~bit;
		break;
	case GPIO_INT_MODE_EDGE:
		data->rising = (trig & GPIO_INT_TRIG_HIGH) != 0 ? data->rising | bit : data->rising & ~bit;
		data->falling = (trig & GPIO_INT_TRIG_LOW) != 0 ? data->falling | bit : data->falling & ~bit;
		data->enabled |= bit;
		break;
#ifdef CONFIG_GPIO_ENABLE_DISABLE_INTERRUPT
	/* These keep a pending edge: only configuring clears it. */
	case GPIO_INT_MODE_DISABLE_ONLY:
		data->enabled &= ~bit;
		k_spin_unlock(&data->lock, key);
		return 0;
	case GPIO_INT_MODE_ENABLE_ONLY:
		data->enabled |= bit;
		k_spin_unlock(&data->lock, key);
		return 0;
#endif
	default:
		k_spin_unlock(&data->lock, key);
		return -ENOTSUP;
	}
	/* Configuring clears the pin's pending edge: it starts from its level now. */
	data->levels = (data->levels & ~bit) | (costar_reg_read(data->registers, REG_INPUTS) & bit);
	k_spin_unlock(&data->lock, key);
	return 0;
}

static int costar_gpio_manage_callback(const struct device *dev, struct gpio_callback *callback, bool set)
{
	struct costar_gpio_data *data = dev->data;

	return gpio_manage_callback(&data->callbacks, callback, set);
}

static void costar_gpio_isr(const void *arg)
{
	const struct device *dev = arg;
	struct costar_gpio_data *data = dev->data;
	k_spinlock_key_t key = k_spin_lock(&data->lock);
	const gpio_port_value_t levels = costar_reg_read(data->registers, REG_INPUTS);
	const gpio_port_pins_t changed = (levels ^ data->levels) & data->inputs & data->enabled;
	const gpio_port_pins_t fired = (changed & levels & data->rising) | (changed & ~levels & data->falling);

	data->levels = levels;
	k_spin_unlock(&data->lock, key);
	if (fired != 0) {
		gpio_fire_callbacks(&data->callbacks, dev, fired);
	}
}

static DEVICE_API(gpio, costar_gpio_api) = {
	.pin_configure = costar_gpio_pin_configure,
	.port_get_raw = costar_gpio_port_get_raw,
	.port_set_masked_raw = costar_gpio_port_set_masked_raw,
	.port_set_bits_raw = costar_gpio_port_set_bits_raw,
	.port_clear_bits_raw = costar_gpio_port_clear_bits_raw,
	.port_toggle_bits = costar_gpio_port_toggle_bits,
	.pin_interrupt_configure = costar_gpio_pin_interrupt_configure,
	.manage_callback = costar_gpio_manage_callback,
};

static int costar_gpio_init(const struct device *dev)
{
	const struct costar_gpio_config *config = dev->config;
	struct costar_gpio_data *data = dev->data;

	data->registers = costar_dev(config->costar_device);
	if (data->registers < 0) {
		LOG_ERR("the board has no registers named %s", config->costar_device);
		return -ENODEV;
	}
	if (costar_dev_irq(data->registers) != config->irq) {
		LOG_ERR("%s raises IRQ %d on the board, not %d", config->costar_device,
			costar_dev_irq(data->registers), config->irq);
		return -EINVAL;
	}
	data->levels = costar_reg_read(data->registers, REG_INPUTS);
	data->driven = costar_reg_read(data->registers, REG_OUTPUTS);
	config->irq_config();
	return 0;
}

#define COSTAR_GPIO_IRQ(n) DT_INST_PROP_OR(n, irq, -1)

#define COSTAR_GPIO_INIT(n)                                                                        \
	static void costar_gpio_irq_config_##n(void)                                               \
	{                                                                                          \
		IF_ENABLED(DT_INST_NODE_HAS_PROP(n, irq), (                                        \
			IRQ_CONNECT(DT_INST_PROP(n, irq), 1, costar_gpio_isr,                      \
				    DEVICE_DT_INST_GET(n), 0);                                     \
			irq_enable(DT_INST_PROP(n, irq));                                          \
		))                                                                                 \
	}                                                                                          \
	static const struct costar_gpio_config costar_gpio_config_##n = {                          \
		.common = {.port_pin_mask = GPIO_PORT_PIN_MASK_FROM_DT_INST(n)},                   \
		.costar_device = DT_INST_PROP(n, costar_device),                                   \
		.irq = COSTAR_GPIO_IRQ(n),                                                         \
		.irq_config = costar_gpio_irq_config_##n,                                          \
	};                                                                                         \
	static struct costar_gpio_data costar_gpio_data_##n;                                       \
	DEVICE_DT_INST_DEFINE(n, costar_gpio_init, NULL, &costar_gpio_data_##n,                    \
			      &costar_gpio_config_##n, PRE_KERNEL_1, CONFIG_GPIO_INIT_PRIORITY,    \
			      &costar_gpio_api);

DT_INST_FOREACH_STATUS_OKAY(COSTAR_GPIO_INIT)
