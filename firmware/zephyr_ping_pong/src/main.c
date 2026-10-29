/*
 * Message-queue ping-pong — an ordinary Zephyr application.
 *
 * main() puts 1..5 into a queue, one every 3 ms.  The receiver thread has the
 * higher priority and blocks on the queue, so each put preempts main():
 * "received n" is printed before "sent n".  Then it exits with status 3,
 * which runs native_sim's exit tasks; one of them exits again with 0, and the
 * program must still end with 3.
 */
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>

#include <nsi_main.h>
#include <posix_native_task.h>

#define MESSAGES 5

K_MSGQ_DEFINE(queue, sizeof(uint32_t), MESSAGES, 4);

static void receiver(void *p1, void *p2, void *p3)
{
	ARG_UNUSED(p1);
	ARG_UNUSED(p2);
	ARG_UNUSED(p3);

	for (int i = 0; i < MESSAGES; i++) {
		uint32_t value;

		k_msgq_get(&queue, &value, K_FOREVER);
		printk("received %u at %lld ms\n", value, k_uptime_get());
	}
}

K_THREAD_DEFINE(receiver_thread, 1024, receiver, NULL, NULL, NULL, 2, 0, 0);

int main(void)
{
	for (uint32_t value = 1; value <= MESSAGES; value++) {
		k_msgq_put(&queue, &value, K_FOREVER);
		printk("sent %u at %lld ms\n", value, k_uptime_get());
		k_msleep(3);
	}
	printk("sender done at %lld ms\n", k_uptime_get());
	nsi_exit(3);
	return 0;
}

static void say_goodbye(void)
{
	printk("exit task ran\n");
	nsi_exit(0);
}

NATIVE_TASK(say_goodbye, ON_EXIT, 1);
