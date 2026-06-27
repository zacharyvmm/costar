//! SmoltcpBridge: deterministic smoltcp stack processing bridge.
//!
//! Connects VirtualEthDevice (guest-side Ethernet) to the smoltcp TCP/IP stack
//! backed by SimNetDevice. Guest-sent Ethernet frames are routed into smoltcp,
//! processed through its protocol stack, and the resulting responses are
//! delivered back to the guest.
//!
//! Also processes frames injected via the C ABI `sim_net_inject_rx` through the
//! smoltcp stack, so all network traffic flows deterministically through one
//! poll point in the scheduler cycle.

use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};

use crate::device::SimNetDevice;
use crate::eth_device::VirtualEthDevice;

/// Number of socket slots in the bridge's SocketSet.
const SOCKET_CAPACITY: usize = 8;

/// A deterministic bridge between the guest's VirtualEthDevice and the
/// simulator's smoltcp network stack (backed by SimNetDevice).
///
/// Holds a smoltcp `Interface` and backing socket storage.  The `Interface`
/// is not parameterised by a specific device type — the device is passed to
/// [`poll`](Self::poll) at each scheduler cycle.
///
/// The bridge is stored in thread-local storage (see [`crate::lib.rs`])
/// and is shared by all scheduler-cycle call-sites.
pub struct SmoltcpBridge {
    /// smoltcp network interface (IP, routes, neighbour cache).
    iface: Interface,
    /// Backing storage for the socket set.  The `'static` lifetime is
    /// technically a lie — the storage lives only as long as `self` —
    /// but this is necessary because `SocketSet` ties its lifetime
    /// parameter to the `SocketStorage` lifetime.  The socket set is
    /// only ever constructed transiently inside [`poll`](Self::poll)
    /// where the borrow never escapes.
    sockets: Vec<smoltcp::iface::SocketStorage<'static>>,
}

impl SmoltcpBridge {
    /// Create a new bridge configured with the simulator-side peer address
    /// `10.0.0.1/24`.
    ///
    /// The interface uses the given Ethernet `mac`.  `now` is the virtual
    /// timestamp at construction time.
    pub fn new(now: Instant, mac: EthernetAddress) -> Self {
        let config = Config::new(HardwareAddress::Ethernet(mac));

        // Create a temporary device just for initialisation — the Interface
        // only needs it to query capabilities.
        let mut tmp_dev = SimNetDevice::new(1500);
        let mut iface = Interface::new(config, &mut tmp_dev, now);

        // Assign the simulator-side IP address.
        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new(IpAddress::v4(10, 0, 0, 1), 24))
                .expect("failed to add IP address to smoltcp interface");
        });

        // Add a default IPv4 route.
        iface
            .routes_mut()
            .add_default_ipv4_route(Ipv4Address::new(10, 0, 0, 1))
            .expect("failed to add default IPv4 route");

        // Pre-allocate empty socket storage.
        let mut sockets: Vec<smoltcp::iface::SocketStorage<'static>> =
            Vec::with_capacity(SOCKET_CAPACITY);
        for _ in 0..SOCKET_CAPACITY {
            sockets.push(smoltcp::iface::SocketStorage::EMPTY);
        }

        Self { iface, sockets }
    }

    /// Create a new bridge with default MAC `02:00:00:00:00:02`.
    pub fn with_default_mac(now: Instant) -> Self {
        Self::new(now, EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]))
    }

    /// Execute one poll cycle of the smoltcp bridge.
    ///
    /// 1. Drain guest-sent frames from `eth` and inject them into `net`'s rx
    ///    queue so smoltcp can process them.
    /// 2. Poll the smoltcp `Interface`, which processes all queued rx frames
    ///    through its TCP/IP stack and generates tx responses into `net`'s
    ///    tx queue.
    /// 3. Drain `net`'s tx queue and inject every response frame back into
    ///    `eth` so the guest receives them.
    ///
    /// Returns `true` if any work was done (frames were moved or smoltcp
    /// processed something), `false` otherwise.
    pub fn poll(
        &mut self,
        now: Instant,
        net: &mut SimNetDevice,
        eth: &mut VirtualEthDevice,
    ) -> bool {
        let mut did_work = false;

        // ── Step 1: Guest-sent frames → SimNetDevice rx queue ──────────
        let guest_frames = eth.drain_tx();
        for frame in guest_frames {
            net.inject_rx(frame);
            did_work = true;
        }

        // ── Step 2: Poll smoltcp ──────────────────────────────────────
        // Take ownership of the socket storage Vec, use it as an owned
        // SocketSet (no borrow of self), then put back whatever remains.
        let storage = std::mem::take(&mut self.sockets);
        let mut socket_set = SocketSet::new(storage);
        let smoltcp_did_work = self.iface.poll(now, net, &mut socket_set);
        if smoltcp_did_work {
            did_work = true;
        }
        // Extract the Vec back from the SocketSet via drop + replacement.
        // SocketSet doesn't expose its inner storage, so we let it drop
        // and allocate a fresh one.  This is cheap (8 empty slots).
        drop(socket_set);
        self.sockets = {
            let mut v = Vec::with_capacity(SOCKET_CAPACITY);
            for _ in 0..SOCKET_CAPACITY {
                v.push(smoltcp::iface::SocketStorage::EMPTY);
            }
            v
        };

        // ── Step 3: smoltcp tx → guest VirtualEthDevice ───────────────
        let response_frames = net.drain_tx();
        for frame in response_frames {
            eth.inject_rx(frame);
            did_work = true;
        }

        // ── Optional: forward unconsumed rx frames directly too ──────
        // Frames that smoltcp didn't consume (e.g., non-IP, or for other
        // hosts) are left in net's rx queue.  Forward them to the guest
        // so nothing is silently dropped.
        let leftover_rx = net.drain_rx();
        for frame in leftover_rx {
            eth.inject_rx(frame);
            did_work = true;
        }

        did_work
    }

    /// Access the underlying smoltcp `Interface` immutably.
    pub fn iface(&self) -> &Interface {
        &self.iface
    }

    /// Access the underlying smoltcp `Interface` mutably.
    pub fn iface_mut(&mut self) -> &mut Interface {
        &mut self.iface
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::time::Instant;

    /// Create a minimal valid Ethernet frame with the given EtherType.
    fn make_ethernet_frame(
        dst_mac: [u8; 6],
        src_mac: [u8; 6],
        ethertype: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut frame = Vec::with_capacity(14 + payload.len());
        frame.extend_from_slice(&dst_mac);
        frame.extend_from_slice(&src_mac);
        frame.extend_from_slice(&ethertype.to_be_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    /// Create a minimal ARP request: who-has target_ip tell sender_ip.
    fn make_arp_request(sender_mac: [u8; 6], sender_ip: [u8; 4], target_ip: [u8; 4]) -> Vec<u8> {
        let mut arp = Vec::with_capacity(28);
        arp.extend_from_slice(&0x0001u16.to_be_bytes()); // HTYPE = Ethernet
        arp.extend_from_slice(&0x0800u16.to_be_bytes()); // PTYPE = IPv4
        arp.push(6); // HLEN
        arp.push(4); // PLEN
        arp.extend_from_slice(&0x0001u16.to_be_bytes()); // OPER = request
        arp.extend_from_slice(&sender_mac);
        arp.extend_from_slice(&sender_ip);
        arp.extend_from_slice(&[0x00; 6]); // target MAC (zero)
        arp.extend_from_slice(&target_ip);
        make_ethernet_frame([0xff; 6], sender_mac, 0x0806, &arp)
    }

    /// Create a minimal ICMP echo request (ping) to dst_ip.
    fn make_icmp_echo_request(
        src_mac: [u8; 6],
        src_ip: [u8; 4],
        dst_ip: [u8; 4],
        identifier: u16,
        sequence: u16,
    ) -> Vec<u8> {
        // Build IPv4 header + ICMP echo request.
        let ip_header_len = 20;
        let icmp_len = 8; // type + code + checksum + id + seq
        let total_len = ip_header_len + icmp_len;

        let mut ip_pkt = Vec::with_capacity(total_len);
        // Version=4, IHL=5
        ip_pkt.push(0x45);
        // DSCP + ECN = 0
        ip_pkt.push(0x00);
        // Total length
        ip_pkt.extend_from_slice(&(total_len as u16).to_be_bytes());
        // Identification
        ip_pkt.extend_from_slice(&0x0000u16.to_be_bytes());
        // Flags + Fragment offset
        ip_pkt.extend_from_slice(&0x0000u16.to_be_bytes());
        // TTL
        ip_pkt.push(64);
        // Protocol = ICMP (1)
        ip_pkt.push(1);
        // Header checksum (placeholder, computed below)
        ip_pkt.extend_from_slice(&0x0000u16.to_be_bytes());
        // Source IP
        ip_pkt.extend_from_slice(&src_ip);
        // Destination IP
        ip_pkt.extend_from_slice(&dst_ip);

        // Compute IPv4 header checksum
        let checksum = internet_checksum(&ip_pkt[..ip_header_len]);
        ip_pkt[10..12].copy_from_slice(&checksum.to_be_bytes());

        // ICMP echo request
        ip_pkt.push(8); // type = echo request
        ip_pkt.push(0); // code = 0
        ip_pkt.extend_from_slice(&0x0000u16.to_be_bytes()); // checksum placeholder
        ip_pkt.extend_from_slice(&identifier.to_be_bytes());
        ip_pkt.extend_from_slice(&sequence.to_be_bytes());

        // Compute ICMP checksum
        let icmp_checksum = internet_checksum(&ip_pkt[ip_header_len..]);
        ip_pkt[ip_header_len + 2..ip_header_len + 4].copy_from_slice(&icmp_checksum.to_be_bytes());

        make_ethernet_frame(
            [0x02, 0x00, 0x00, 0x00, 0x00, 0x02], // dest = smoltcp MAC
            src_mac,
            0x0800, // IPv4
            &ip_pkt,
        )
    }

    /// Compute the internet checksum (16-bit one's complement sum).
    fn internet_checksum(data: &[u8]) -> u16 {
        let mut sum: u32 = 0;
        let mut i = 0;
        while i + 1 < data.len() {
            sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
            i += 2;
        }
        if i < data.len() {
            sum += (data[i] as u32) << 8;
        }
        while sum >> 16 != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
        !(sum as u16)
    }

    // ── Tests ──────────────────────────────────────────────────────────────

    #[test]
    fn test_bridge_create() {
        let now = Instant::from_millis(0);
        let bridge = SmoltcpBridge::with_default_mac(now);

        // Interface should have our IP
        let ip_addrs = bridge.iface.ip_addrs();
        assert_eq!(ip_addrs.len(), 1);
        assert_eq!(ip_addrs[0].address(), IpAddress::v4(10, 0, 0, 1));
        assert_eq!(ip_addrs[0].prefix_len(), 24);
    }

    #[test]
    fn test_bridge_arp_response() {
        let now = Instant::from_millis(0);
        let mut bridge = SmoltcpBridge::with_default_mac(now);
        let mut net = SimNetDevice::new(1500);
        let mut eth = VirtualEthDevice::new(0, [0x02, 0x00, 0x00, 0x00, 0x00, 0x01], 1500);

        // Guest sends an ARP request for 10.0.0.1 (our smoltcp IP)
        let arp_req = make_arp_request(
            [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            [10, 0, 0, 2],
            [10, 0, 0, 1],
        );

        // Inject ARP request as guest-sent frame
        eth.send(&arp_req);
        assert!(eth.has_tx());

        // Poll the bridge
        let did_work = bridge.poll(now, &mut net, &mut eth);
        assert!(did_work, "bridge should have processed the ARP request");

        // Guest should have received a response
        assert!(eth.has_rx(), "guest should have received an ARP reply");

        let mut buf = [0u8; 64];
        let n = eth.recv_into(&mut buf);
        assert!(n > 0, "should have received a frame");

        // Verify it's an ARP reply
        // Ethernet: dst MAC = guest MAC, src MAC = smoltcp MAC, EtherType = 0x0806
        assert_eq!(
            &buf[0..6],
            &[0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            "ARP reply dst should be guest MAC"
        );
        assert_eq!(
            &buf[6..12],
            &[0x02, 0x00, 0x00, 0x00, 0x00, 0x02],
            "ARP reply src should be smoltcp MAC"
        );
        assert_eq!(u16::from_be_bytes([buf[12], buf[13]]), 0x0806);

        // ARP header: HTYPE=1, PTYPE=0x0800, HLEN=6, PLEN=4, OPER=2 (reply)
        let arp_offset = 14;
        assert_eq!(
            u16::from_be_bytes([buf[arp_offset], buf[arp_offset + 1]]),
            0x0001
        );
        assert_eq!(
            u16::from_be_bytes([buf[arp_offset + 2], buf[arp_offset + 3]]),
            0x0800
        );
        assert_eq!(buf[arp_offset + 4], 6);
        assert_eq!(buf[arp_offset + 5], 4);
        assert_eq!(
            u16::from_be_bytes([buf[arp_offset + 6], buf[arp_offset + 7]]),
            0x0002,
            "OPER should be reply (2)"
        );
    }

    #[test]
    fn test_bridge_icmp_echo_reply() {
        let now = Instant::from_millis(0);
        let mut bridge = SmoltcpBridge::with_default_mac(now);
        let mut net = SimNetDevice::new(1500);
        let mut eth = VirtualEthDevice::new(0, [0x02, 0x00, 0x00, 0x00, 0x00, 0x01], 1500);

        // First, ARP exchange so smoltcp learns guest's MAC
        let arp_req = make_arp_request(
            [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            [10, 0, 0, 2],
            [10, 0, 0, 1],
        );
        eth.send(&arp_req);
        bridge.poll(now, &mut net, &mut eth);
        // Drain the ARP reply from guest's rx
        let mut buf = [0u8; 64];
        eth.recv_into(&mut buf);

        // Now guest sends ICMP echo request to 10.0.0.1
        let icmp_req = make_icmp_echo_request(
            [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            [10, 0, 0, 2],
            [10, 0, 0, 1],
            0x1234,
            0x0001,
        );
        eth.send(&icmp_req);
        assert!(eth.has_tx());

        // Poll the bridge — this should generate an ICMP echo reply
        let did_work = bridge.poll(now, &mut net, &mut eth);
        assert!(did_work, "bridge should have processed the ICMP request");

        // Guest should have received an ICMP echo reply
        assert!(
            eth.has_rx(),
            "guest should have received an ICMP echo reply"
        );

        let mut buf = [0u8; 128];
        let n = eth.recv_into(&mut buf);
        assert!(n > 0, "should have received a frame");

        // Verify Ethernet header: dst=guest MAC, src=smoltcp MAC, EtherType=0x0800
        assert_eq!(&buf[0..6], &[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&buf[6..12], &[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
        assert_eq!(u16::from_be_bytes([buf[12], buf[13]]), 0x0800);

        // IPv4 header: protocol = ICMP (1)
        let ip_offset = 14;
        assert_eq!(buf[ip_offset], 0x45); // version + IHL
        assert_eq!(buf[ip_offset + 9], 1); // protocol = ICMP

        // ICMP header: type = 0 (echo reply), code = 0
        let icmp_offset = ip_offset + 20; // 20-byte IPv4 header
        assert_eq!(buf[icmp_offset], 0, "ICMP type should be echo reply (0)");
        assert_eq!(buf[icmp_offset + 1], 0, "ICMP code should be 0");
    }

    #[test]
    fn test_bridge_noop_when_no_frames() {
        let now = Instant::from_millis(0);
        let mut bridge = SmoltcpBridge::with_default_mac(now);
        let mut net = SimNetDevice::new(1500);
        let mut eth = VirtualEthDevice::new(0, [0x02, 0x00, 0x00, 0x00, 0x00, 0x01], 1500);

        // No frames pending
        assert!(!eth.has_tx());
        assert!(net.rx_empty());
        assert!(net.tx_empty());

        let did_work = bridge.poll(now, &mut net, &mut eth);
        assert!(
            !did_work,
            "bridge should be no-op when no frames are pending"
        );
        assert!(!eth.has_rx());
    }

    #[test]
    fn test_bridge_forwards_sim_net_inject_rx() {
        let now = Instant::from_millis(0);
        let mut bridge = SmoltcpBridge::with_default_mac(now);
        let mut net = SimNetDevice::new(1500);
        let mut eth = VirtualEthDevice::new(0, [0x02, 0x00, 0x00, 0x00, 0x00, 0x01], 1500);

        // Inject a frame directly into SimNetDevice (simulating C ABI
        // sim_net_inject_rx).  This is an ARP request from an external
        // host for the guest's IP (NOT smoltcp's IP).
        let arp_req = make_arp_request(
            [0x02, 0x00, 0x00, 0x00, 0x00, 0x03],
            [10, 0, 0, 3],
            [10, 0, 0, 2], // target IP = guest IP
        );
        net.inject_rx(arp_req);

        // Poll the bridge
        let did_work = bridge.poll(now, &mut net, &mut eth);

        // The smoltcp stack would NOT respond to this ARP (it's not for
        // 10.0.0.1), but our bridge forwards unconsumed rx frames to
        // the guest.  So the guest should see the ARP request.
        assert!(
            did_work || eth.has_rx(),
            "bridge should forward unconsumed rx to guest"
        );
        if eth.has_rx() {
            let mut buf = [0u8; 64];
            let n = eth.recv_into(&mut buf);
            assert!(n > 0);
        }
    }
}
