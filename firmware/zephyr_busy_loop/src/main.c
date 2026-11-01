/*
 * Busy loop — main() spins until a timer's expiry function (an ISR) sets a
 * flag, 5 ms after boot.  On costar's infinitely fast CPU the spin only ends
 * if the build is instrumented (costar_add_zephyr_image(... INSTRUMENT)):
 * the spin then takes time, and the timer interrupt preempts it.
 */
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>

static volatile bool expired;

static void on_expiry(struct k_timer *timer)
{
	ARG_UNUSED(timer);
	expired = true;
}

K_TIMER_DEFINE(timer, on_expiry, NULL);

int main(void)
{
	k_timer_start(&timer, K_MSEC(5), K_NO_WAIT);
	while (!expired) {
	}
	printk("spin ended at %lld ms\n", k_uptime_get());
	return 0;
}
