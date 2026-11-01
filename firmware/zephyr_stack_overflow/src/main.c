/*
 * Stack overflow — a thread with a 1 KiB stack copies a 32 KiB message into
 * a buffer on its stack.  On a microcontroller that overwrites whatever lies
 * below the stack; costar stops the machine at the thread's next costar
 * call (here, printing), so "still running" is never printed.
 */
#include <string.h>

#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>

/* Volatile, as a length read from a message would be. */
static volatile size_t message_length = 32 * 1024;

static void parse(void *p1, void *p2, void *p3)
{
	ARG_UNUSED(p1);
	ARG_UNUSED(p2);
	ARG_UNUSED(p3);

	const size_t length = message_length;
	char message[length];

	memset(message, 'm', length);
	printk("copied %u bytes\n", (unsigned int)length + message[length - 1] - 'm');
	k_msleep(1);
	printk("still running\n");
}

K_THREAD_DEFINE(parser, 1024, parse, NULL, NULL, NULL, 5, 0, 0);

int main(void)
{
	printk("main done\n");
	return 0;
}
