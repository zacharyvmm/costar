/*
 * The data model of a Zephyr image (data_model.c), printed: board native_sim
 * is ILP32, native_sim/native/64 is LP64.
 */
#include <zephyr/sys/printk.h>

#include "data_model.h"

static void print(const char *label, uint32_t value)
{
	printk("%s %u\n", label, value);
}

int main(void)
{
	data_model_report(print);
	return 0;
}
