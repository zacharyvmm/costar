/*
 * A dimmer — an ordinary Zephyr application that uses Zephyr's ADC, PWM and
 * EEPROM APIs on costar devices (see app.overlay).
 *
 * At boot it counts boots in EEPROM and says what the knob was last set to.
 * On the first boot it also checks what the drivers refuse, reads the
 * battery once, and samples the knob four times, a millisecond apart, with
 * an asynchronous read.  Then, every millisecond, it reads the knob (ADC
 * channel 0) and sets the lamp's duty cycle (PWM channel 0) to match,
 * saving each new knob position in EEPROM.  A status LED (PWM channel 1,
 * active low) glows at a quarter.
 */
#include <zephyr/drivers/adc.h>
#include <zephyr/drivers/eeprom.h>
#include <zephyr/drivers/pwm.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>

#define USER DT_PATH(zephyr_user)

static const struct adc_dt_spec knob = ADC_DT_SPEC_GET_BY_NAME(USER, knob);
static const struct adc_dt_spec battery = ADC_DT_SPEC_GET_BY_NAME(USER, battery);
static const struct pwm_dt_spec lamp = PWM_DT_SPEC_GET_BY_NAME(USER, lamp);
static const struct pwm_dt_spec led = PWM_DT_SPEC_GET_BY_NAME(USER, led);
static const struct device *const eeprom = DEVICE_DT_GET(DT_ALIAS(eeprom_0));

#define BOOTS_AT 0
#define KNOB_AT 4

static int read_sample(const struct adc_dt_spec *spec, uint16_t *sample)
{
	struct adc_sequence sequence = {.buffer = sample, .buffer_size = sizeof(*sample)};

	adc_sequence_init_dt(spec, &sequence);
	return adc_read_dt(spec, &sequence);
}

static void check_refusals(void)
{
	struct adc_channel_cfg differential = {
		.gain = ADC_GAIN_1, .reference = ADC_REF_INTERNAL, .channel_id = 2, .differential = 1};
	struct adc_channel_cfg missing = {.gain = ADC_GAIN_1, .reference = ADC_REF_INTERNAL, .channel_id = 9};
	uint16_t sample;
	struct adc_sequence unset = {.channels = BIT(2), .buffer = &sample, .buffer_size = 2, .resolution = 12};
	struct adc_sequence too_fine = {.channels = BIT(0), .buffer = &sample, .buffer_size = 2, .resolution = 17};
	struct adc_sequence too_small = {.channels = BIT(0), .buffer = &sample, .buffer_size = 1, .resolution = 12};

	printk("adc: differential %d, channel 9 %d, unset channel %d, 17 bits %d, small buffer %d\n",
	       adc_channel_setup(knob.dev, &differential), adc_channel_setup(knob.dev, &missing),
	       adc_read(knob.dev, &unset), adc_read(knob.dev, &too_fine), adc_read(knob.dev, &too_small));

	printk("pwm: pulse over period %d, channel 8 %d, other flags %d\n",
	       pwm_set_cycles(lamp.dev, 0, 100, 200, 0), pwm_set_cycles(lamp.dev, 8, 100, 50, 0),
	       pwm_set_cycles(lamp.dev, 0, 100, 50, BIT(8)));

	/* Unlike flash, a byte can be written over without erasing. */
	const uint8_t first = 0x12;
	const uint8_t second = 0x34;
	uint8_t byte = 0;
	int written = eeprom_write(eeprom, 8, &first, 1);

	written = written != 0 ? written : eeprom_write(eeprom, 8, &second, 1);

	eeprom_read(eeprom, 8, &byte, 1);
	printk("eeprom: %zu bytes, rewritten %d to 0x%02x, past the end %d\n", eeprom_get_size(eeprom),
	       written, byte, eeprom_write(eeprom, eeprom_get_size(eeprom) - 1, &first, 2));
}

static enum adc_action on_sampling(const struct device *dev, const struct adc_sequence *sequence,
				   uint16_t index)
{
	const uint16_t *samples = sequence->buffer;

	ARG_UNUSED(dev);
	printk("sampling %u at %u us: knob %u\n", index, k_cyc_to_us_floor32(k_cycle_get_32()),
	       samples[index]);
	return ADC_ACTION_CONTINUE;
}

/* Four samplings of the knob, a millisecond apart, without blocking. */
static void sample_knob_four_times(void)
{
	uint16_t samples[4];
	const struct adc_sequence_options options = {
		.interval_us = 1000, .extra_samplings = 3, .callback = on_sampling};
	struct adc_sequence sequence = {.buffer = samples, .buffer_size = sizeof(samples)};
	struct k_poll_signal done;
	struct k_poll_event event = K_POLL_EVENT_INITIALIZER(K_POLL_TYPE_SIGNAL, K_POLL_MODE_NOTIFY_ONLY, &done);

	adc_sequence_init_dt(&knob, &sequence);
	sequence.options = &options;
	k_poll_signal_init(&done);
	printk("async read: %d\n", adc_read_async(knob.dev, &sequence, &done));
	k_poll(&event, 1, K_FOREVER);
	printk("async read done: %d\n", done.result);
}

int main(void)
{
	if (!adc_is_ready_dt(&knob) || !pwm_is_ready_dt(&lamp) || !device_is_ready(eeprom)) {
		printk("devices not ready\n");
		return 1;
	}
	uint32_t boots;
	uint16_t saved;

	eeprom_read(eeprom, BOOTS_AT, &boots, sizeof(boots));
	boots = boots == UINT32_MAX ? 1 : boots + 1;
	eeprom_write(eeprom, BOOTS_AT, &boots, sizeof(boots));
	eeprom_read(eeprom, KNOB_AT, &saved, sizeof(saved));
	if (saved == UINT16_MAX) {
		printk("boot %u, no knob saved\n", boots);
	} else {
		printk("boot %u, knob saved at %u\n", boots, saved);
	}

	adc_channel_setup_dt(&knob);
	adc_channel_setup_dt(&battery);
	pwm_set_pulse_dt(&led, led.period / 4);
	if (boots == 1) {
		uint16_t level;

		check_refusals();
		read_sample(&battery, &level);
		int32_t mv = level;

		adc_raw_to_millivolts_dt(&battery, &mv);
		printk("battery %u: %d mV\n", level, mv);
		sample_knob_four_times();
	}

	uint16_t shown = UINT16_MAX;

	for (int64_t at = k_uptime_get();; at++) {
		uint16_t sample;

		k_sleep(K_TIMEOUT_ABS_MS(at));
		read_sample(&knob, &sample);
		if (sample == shown) {
			continue;
		}
		const uint32_t pulse = (uint64_t)lamp.period * sample / BIT_MASK(knob.resolution);
		int32_t mv = sample;

		shown = sample;
		adc_raw_to_millivolts_dt(&knob, &mv);
		pwm_set_pulse_dt(&lamp, pulse);
		eeprom_write(eeprom, KNOB_AT, &sample, sizeof(sample));
		printk("knob %u (%d mV): lamp %u ns of %u\n", sample, mv, pulse, lamp.period);
	}
}
