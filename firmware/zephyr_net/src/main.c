/*
 * UDP echo and ping over Zephyr's own network stack — an ordinary Zephyr
 * application on a costar Ethernet interface (see app.overlay).
 *
 * The machine's static IPv4 configuration is in EEPROM, as a factory would
 * provision it (the scenario writes it):
 *
 *   bytes 0-3  this machine's address, in a /24
 *   bytes 4-7  its peer's address
 *   byte 8     1 if this machine is the client
 *
 * Every machine echoes UDP datagrams on port 7, and the stack answers
 * pings.  The client pings its peer twice, then sends it two datagrams, and
 * says how long each answer took.
 */
#include <string.h>

#include <zephyr/drivers/eeprom.h>
#include <zephyr/kernel.h>
#include <zephyr/net/icmp.h>
#include <zephyr/net/net_if.h>
#include <zephyr/net/net_ip.h>
#include <zephyr/net/socket.h>
#include <zephyr/sys/printk.h>

#define ECHO_PORT 7

static const struct device *const eeprom = DEVICE_DT_GET(DT_ALIAS(eeprom_0));

static struct {
	uint8_t self[4];
	uint8_t peer[4];
	uint8_t client;
} settings;

K_SEM_DEFINE(configured, 0, 1);
K_SEM_DEFINE(pong, 0, 1);

static uint32_t now_us(void)
{
	return k_cyc_to_us_floor32(k_cycle_get_32());
}

static void echo(void *a, void *b, void *c)
{
	ARG_UNUSED(a);
	ARG_UNUSED(b);
	ARG_UNUSED(c);
	k_sem_take(&configured, K_FOREVER);

	const int sock = zsock_socket(NET_AF_INET, NET_SOCK_DGRAM, NET_IPPROTO_UDP);
	struct net_sockaddr_in local = {.sin_family = NET_AF_INET, .sin_port = net_htons(ECHO_PORT)};

	if (sock < 0 || zsock_bind(sock, (struct net_sockaddr *)&local, sizeof(local)) < 0) {
		printk("echo: no socket\n");
		return;
	}
	for (;;) {
		char buffer[64];
		struct net_sockaddr_in from;
		net_socklen_t from_length = sizeof(from);
		const ssize_t length =
			zsock_recvfrom(sock, buffer, sizeof(buffer), 0, (struct net_sockaddr *)&from, &from_length);

		if (length < 0) {
			continue;
		}
		char address[NET_INET_ADDRSTRLEN];

		net_addr_ntop(NET_AF_INET, &from.sin_addr, address, sizeof(address));
		printk("echo %d bytes to %s:%u\n", (int)length, address, net_ntohs(from.sin_port));
		zsock_sendto(sock, buffer, length, 0, (struct net_sockaddr *)&from, from_length);
	}
}

K_THREAD_DEFINE(echo_thread, 2048, echo, NULL, NULL, NULL, 7, 0, 0);

static enum net_verdict on_echo_reply(struct net_icmp_ctx *ctx, struct net_pkt *pkt,
				      struct net_icmp_ip_hdr *ip_hdr, struct net_icmp_hdr *icmp_hdr,
				      void *user_data)
{
	ARG_UNUSED(ctx);
	ARG_UNUSED(pkt);
	ARG_UNUSED(ip_hdr);
	ARG_UNUSED(icmp_hdr);
	ARG_UNUSED(user_data);
	k_sem_give(&pong);
	return NET_OK;
}

static void ping(struct net_if *iface, struct net_sockaddr_in *peer)
{
	struct net_icmp_ctx ctx;

	net_icmp_init_ctx(&ctx, NET_AF_INET, NET_ICMPV4_ECHO_REPLY, 0, on_echo_reply);
	for (uint16_t sequence = 1; sequence <= 2; sequence++) {
		struct net_icmp_ping_params params = {
			.identifier = 1, .sequence = sequence, .data = "costar", .data_size = 6};
		const uint32_t start = now_us();
		const int sent = net_icmp_send_echo_request(&ctx, iface, (struct net_sockaddr *)peer, &params, NULL);

		if (sent == 0 && k_sem_take(&pong, K_MSEC(100)) == 0) {
			printk("ping %u: reply after %u us\n", sequence, now_us() - start);
		} else {
			printk("ping %u: no reply (%d)\n", sequence, sent);
		}
	}
	net_icmp_cleanup_ctx(&ctx);
}

static void send_datagrams(struct net_sockaddr_in *peer)
{
	const int sock = zsock_socket(NET_AF_INET, NET_SOCK_DGRAM, NET_IPPROTO_UDP);
	const struct zsock_timeval timeout = {.tv_usec = 100000};

	zsock_setsockopt(sock, ZSOCK_SOL_SOCKET, ZSOCK_SO_RCVTIMEO, &timeout, sizeof(timeout));
	for (int i = 1; i <= 2; i++) {
		char message[16];
		char reply[16] = "";
		const int length = snprintk(message, sizeof(message), "hello %d", i);
		const uint32_t start = now_us();

		zsock_sendto(sock, message, length, 0, (struct net_sockaddr *)peer, sizeof(*peer));
		const ssize_t received = zsock_recv(sock, reply, sizeof(reply) - 1, 0);

		if (received < 0) {
			printk("udp '%s': no reply\n", message);
		} else {
			printk("udp '%s': '%s' after %u us\n", message, reply, now_us() - start);
		}
	}
	zsock_close(sock);
}

int main(void)
{
	struct net_if *iface = net_if_get_default();

	if (!device_is_ready(eeprom) || iface == NULL) {
		printk("no EEPROM or no network interface\n");
		return 1;
	}
	eeprom_read(eeprom, 0, &settings, sizeof(settings));

	struct net_in_addr self;
	const struct net_in_addr netmask = {.s4_addr = {255, 255, 255, 0}};
	char address[NET_INET_ADDRSTRLEN];
	const uint8_t *mac = net_if_get_link_addr(iface)->addr;

	memcpy(&self, settings.self, sizeof(self));
	net_if_ipv4_addr_add(iface, &self, NET_ADDR_MANUAL, 0);
	net_if_ipv4_set_netmask_by_addr(iface, &self, &netmask);
	net_addr_ntop(NET_AF_INET, &self, address, sizeof(address));
	printk("%s at %02x:%02x:%02x:%02x:%02x:%02x\n", address, mac[0], mac[1], mac[2], mac[3], mac[4],
	       mac[5]);
	k_sem_give(&configured);

	if (settings.client == 1) {
		struct net_sockaddr_in peer = {.sin_family = NET_AF_INET, .sin_port = net_htons(ECHO_PORT)};

		memcpy(&peer.sin_addr, settings.peer, sizeof(peer.sin_addr));
		ping(iface, &peer);
		send_datagrams(&peer);
	}
	return 0;
}
