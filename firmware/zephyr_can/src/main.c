/*
 * CAN echo — an ordinary Zephyr application that uses Zephyr's CAN API on a
 * costar device (see app.overlay).  It announces itself with frame 0x100,
 * then answers each frame it receives for standard ID 0x200 or extended ID
 * 0x12345678 with the next ID and its first data byte plus one.  Built with
 * CONFIG_CAN_FD_MODE, it runs in CAN FD mode and answers FD frames too.
 *
 * First, for two milliseconds, it checks loopback mode (in classic mode): its
 * own frames for 0x7ff come back through a filter that removes itself and
 * answers with 0x7fd from the ISR (on the wire after the frame it answers),
 * and frames for 0x7fe from the scenario are counted.  Then, with interrupts
 * locked, each TX callback sends the next of twelve frames for 0x7fc: more
 * than the loopback queue holds, so a send waits for the ISR to drain it.
 */
#include <zephyr/drivers/can.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>

static const struct device *const can = DEVICE_DT_GET(DT_CHOSEN(zephyr_canbus));

CAN_MSGQ_DEFINE(received, 8);

static int once_filter;
static int once_calls, once_in_isr, counted, counted_fd, chained, chain_left;
static const struct can_frame link = {.id = 0x7fc, .dlc = 1, .data = {0x0c}};

static void sent(const struct device *dev, int error, void *user_data)
{
	ARG_UNUSED(dev);
	ARG_UNUSED(error);
	ARG_UNUSED(user_data);
}

static void once(const struct device *dev, struct can_frame *frame, void *user_data)
{
	const struct can_frame answer = {.id = 0x7fd, .dlc = 1, .data = {0xaa}};

	ARG_UNUSED(frame);
	ARG_UNUSED(user_data);
	once_calls++;
	once_in_isr += k_is_in_isr();
	can_remove_rx_filter(dev, once_filter);
	can_send(dev, &answer, K_NO_WAIT, sent, NULL);
}

static void count(const struct device *dev, struct can_frame *frame, void *user_data)
{
	ARG_UNUSED(dev);
	ARG_UNUSED(user_data);
	counted++;
	counted_fd += (frame->flags & CAN_FRAME_FDF) != 0;
}

static void count_chained(const struct device *dev, struct can_frame *frame, void *user_data)
{
	ARG_UNUSED(dev);
	ARG_UNUSED(frame);
	ARG_UNUSED(user_data);
	chained++;
}

static void chain(const struct device *dev, int error, void *user_data)
{
	ARG_UNUSED(error);
	ARG_UNUSED(user_data);
	if (chain_left-- > 0) {
		can_send(dev, &link, K_FOREVER, chain, NULL);
	}
}

static void check_loopback(void)
{
	const struct can_filter mine = {.id = 0x7ff, .mask = CAN_STD_ID_MASK};
	const struct can_filter theirs = {.id = 0x7fe, .mask = CAN_STD_ID_MASK};
	const struct can_frame frame = {.id = 0x7ff, .dlc = 1, .data = {0x55}};
	const struct can_filter links = {.id = 0x7fc, .mask = CAN_STD_ID_MASK};
	int counter;
	int chain_counter;
	unsigned int key;

	can_set_mode(can, CAN_MODE_LOOPBACK);
	can_start(can);
	once_filter = can_add_rx_filter(can, once, NULL, &mine);
	counter = can_add_rx_filter(can, count, NULL, &theirs);
	can_send(can, &frame, K_FOREVER, NULL, NULL);
	can_send(can, &frame, K_FOREVER, NULL, NULL);
	chain_counter = can_add_rx_filter(can, count_chained, NULL, &links);
	chain_left = 11;
	key = irq_lock();
	can_send(can, &link, K_FOREVER, chain, NULL);
	irq_unlock(key);
	k_sleep(K_TIMEOUT_ABS_MS(2));
	printk("loopback: once called %d times (%d in an ISR); counted %d frames (%d FD); %d chained\n",
	       once_calls, once_in_isr, counted, counted_fd, chained);
	can_remove_rx_filter(can, counter);
	can_remove_rx_filter(can, chain_counter);
	can_stop(can);
}

int main(void)
{
	const struct can_filter standard = {.id = 0x200, .mask = CAN_STD_ID_MASK};
	const struct can_filter extended = {.id = 0x12345678, .mask = CAN_EXT_ID_MASK, .flags = CAN_FILTER_IDE};
	struct can_frame frame = {.id = 0x100, .dlc = 2, .data = {0xb0, 0x07}};
	const struct can_frame brs_only = {.id = 0x101, .flags = CAN_FRAME_BRS};
	const struct can_frame fd_remote = {.id = 0x101, .flags = CAN_FRAME_FDF | CAN_FRAME_RTR};

	if (!device_is_ready(can)) {
		printk("can not ready\n");
		return 1;
	}
	check_loopback();
	if (can_set_mode(can, IS_ENABLED(CONFIG_CAN_FD_MODE) ? CAN_MODE_FD : CAN_MODE_NORMAL) != 0 ||
	    can_start(can) != 0) {
		printk("can not ready\n");
		return 1;
	}
	printk("send BRS without FD: %d; FD remote frame: %d\n", can_send(can, &brs_only, K_FOREVER, NULL, NULL),
	       can_send(can, &fd_remote, K_FOREVER, NULL, NULL));
	can_add_rx_filter_msgq(can, &received, &standard);
	can_add_rx_filter_msgq(can, &received, &extended);
	can_send(can, &frame, K_FOREVER, NULL, NULL);

	for (;;) {
		k_msgq_get(&received, &frame, K_FOREVER);
		printk("got %x%s%s, %u bytes, at %u ms\n", frame.id, (frame.flags & CAN_FRAME_IDE) ? " (extended)" : "",
		       (frame.flags & CAN_FRAME_FDF) ? " (FD)" : "", can_dlc_to_bytes(frame.dlc), k_uptime_get_32());
		frame.id += 1;
		frame.data[0] += 1;
		can_send(can, &frame, K_FOREVER, NULL, NULL);
	}
}
