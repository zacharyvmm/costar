/*
 * CAN echo — an ordinary Zephyr application that uses Zephyr's CAN API on a
 * costar device (see app.overlay).  It announces itself with frame 0x100,
 * then answers each frame it receives for standard ID 0x200 or extended ID
 * 0x12345678 with the next ID and its first data byte plus one.  Built with
 * CONFIG_CAN_FD_MODE, it runs in CAN FD mode and answers FD frames too.
 */
#include <zephyr/drivers/can.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>

static const struct device *const can = DEVICE_DT_GET(DT_CHOSEN(zephyr_canbus));

CAN_MSGQ_DEFINE(received, 8);

int main(void)
{
	const struct can_filter standard = {.id = 0x200, .mask = CAN_STD_ID_MASK};
	const struct can_filter extended = {.id = 0x12345678, .mask = CAN_EXT_ID_MASK, .flags = CAN_FILTER_IDE};
	struct can_frame frame = {.id = 0x100, .dlc = 2, .data = {0xb0, 0x07}};

	if (!device_is_ready(can) || (IS_ENABLED(CONFIG_CAN_FD_MODE) && can_set_mode(can, CAN_MODE_FD) != 0) ||
	    can_start(can) != 0) {
		printk("can not ready\n");
		return 1;
	}
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
