//! Virtual HCI Controller for Bluetooth simulation.
//!
#![allow(missing_docs)]
//
// Replaces the hardware HCI transport (UART/SPI/USB) with a deterministic
// in-process controller.  Zephyr's BT host communicates with this controller
// via HCI command/event packets.

use std::collections::{BTreeMap, VecDeque};

/// HCI packet type indicator (1-byte header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HciPacketType {
    Command = 1,
    AclData = 2,
    ScoData = 3,
    Event = 4,
    IsoData = 5,
}

impl HciPacketType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Command),
            2 => Some(Self::AclData),
            3 => Some(Self::ScoData),
            4 => Some(Self::Event),
            5 => Some(Self::IsoData),
            _ => None,
        }
    }

    pub fn to_u8(&self) -> u8 {
        match self {
            Self::Command => 1,
            Self::AclData => 2,
            Self::ScoData => 3,
            Self::Event => 4,
            Self::IsoData => 5,
        }
    }
}

/// A raw HCI packet (type byte + payload).
#[derive(Debug, Clone)]
pub struct HciPacket {
    pub packet_type: u8,
    pub payload: Vec<u8>,
}

impl HciPacket {
    pub fn new(packet_type: u8, payload: Vec<u8>) -> Self {
        Self {
            packet_type,
            payload,
        }
    }
}

/// Minimum HCI command set supported for the MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HciCommand {
    Reset = 0x0C03,
    SetAdvertisingData = 0x2008,
    SetAdvertisingParameters = 0x2006,
    SetAdvertisingEnable = 0x200A,
    CreateConnection = 0x200D,
    Disconnect = 0x0406,
    ReadLocalSupportedFeatures = 0x1003,
}

impl HciCommand {
    pub fn from_u16(opcode: u16) -> Option<Self> {
        match opcode {
            0x0C03 => Some(Self::Reset),
            0x2008 => Some(Self::SetAdvertisingData),
            0x2006 => Some(Self::SetAdvertisingParameters),
            0x200A => Some(Self::SetAdvertisingEnable),
            0x200D => Some(Self::CreateConnection),
            0x0406 => Some(Self::Disconnect),
            0x1003 => Some(Self::ReadLocalSupportedFeatures),
            _ => None,
        }
    }
}

/// A deterministic virtual HCI controller.
pub struct VirtualHciController {
    pub id: u32,
    /// HCI commands received from the host (not yet processed).
    cmd_queue: VecDeque<HciPacket>,
    /// HCI events to deliver to the host.
    event_queue: VecDeque<HciPacket>,
    /// ACL data from host (to be delivered to peer).
    acl_host_tx: VecDeque<HciPacket>,
    /// ACL data for host (from peer).
    acl_host_rx: VecDeque<HciPacket>,
    /// Whether the controller is advertising.
    pub advertising: bool,
    /// Connected peer address (if any).
    pub connected_peer: Option<[u8; 6]>,
    /// Scripted responses: (command_opcode → response_event_payload).
    pub script: BTreeMap<u16, Vec<u8>>,
    /// Receive callback registered by the HCI driver.
    pub rx_callback: Option<unsafe extern "C" fn()>,
}

impl VirtualHciController {
    pub fn new(id: u32) -> Self {
        Self {
            id,
            cmd_queue: VecDeque::new(),
            event_queue: VecDeque::new(),
            acl_host_tx: VecDeque::new(),
            acl_host_rx: VecDeque::new(),
            advertising: false,
            connected_peer: None,
            script: BTreeMap::new(),
            rx_callback: None,
        }
    }

    /// Host sends an HCI command or ACL data packet.
    pub fn send(&mut self, packet_type: u8, data: &[u8]) {
        let pkt = HciPacket::new(packet_type, data.to_vec());
        match HciPacketType::from_u8(packet_type) {
            Some(HciPacketType::Command) => {
                self.cmd_queue.push_back(pkt);
            }
            Some(HciPacketType::AclData) => {
                self.acl_host_tx.push_back(pkt);
            }
            _ => {} // SCO/ISO not supported in MVP
        }
    }

    /// Host reads the next HCI event or ACL data packet.
    /// Writes packet_type into the first byte of buf, then payload.
    /// Returns total bytes written (1 + payload_len), or 0 if nothing pending.
    pub fn recv_into(&mut self, buf: &mut [u8]) -> usize {
        // Prioritize events over ACL data
        if let Some(pkt) = self.event_queue.pop_front() {
            return self.write_packet_to_buf(&pkt, buf);
        }
        if let Some(pkt) = self.acl_host_rx.pop_front() {
            return self.write_packet_to_buf(&pkt, buf);
        }
        0
    }

    fn write_packet_to_buf(&self, pkt: &HciPacket, buf: &mut [u8]) -> usize {
        if buf.is_empty() {
            return 0;
        }
        buf[0] = pkt.packet_type;
        let payload_len = pkt.payload.len().min(buf.len() - 1);
        buf[1..1 + payload_len].copy_from_slice(&pkt.payload[..payload_len]);
        1 + payload_len
    }

    /// Inject a scripted HCI event from the test harness.
    pub fn inject_event(&mut self, packet_type: u8, data: &[u8]) {
        self.event_queue
            .push_back(HciPacket::new(packet_type, data.to_vec()));
    }

    /// Process pending HCI commands from the host.
    /// Returns the number of commands processed.
    pub fn process_commands(&mut self) -> usize {
        let mut processed = 0;
        while let Some(cmd) = self.cmd_queue.pop_front() {
            processed += 1;
            // Look up in script first
            if cmd.payload.len() >= 2 {
                let opcode = u16::from_le_bytes([cmd.payload[0], cmd.payload[1]]);
                if let Some(response) = self.script.remove(&opcode) {
                    self.event_queue
                        .push_back(HciPacket::new(HciPacketType::Event.to_u8(), response));
                    continue;
                }
            }
            // Default: respond with CommandComplete (status=0) for known commands
            self.handle_default_command(&cmd);
        }
        processed
    }

    fn handle_default_command(&mut self, cmd: &HciPacket) {
        if cmd.payload.len() < 2 {
            return;
        }
        let opcode = u16::from_le_bytes([cmd.payload[0], cmd.payload[1]]);

        match HciCommand::from_u16(opcode) {
            Some(HciCommand::Reset) => {
                // CommandComplete(HCI_Reset, Status=0)
                self.event_queue
                    .push_back(HciPacket::new(4, vec![0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00]));
            }
            Some(HciCommand::SetAdvertisingData) => {
                self.event_queue
                    .push_back(HciPacket::new(4, vec![0x0E, 0x04, 0x01, 0x08, 0x20, 0x00]));
            }
            Some(HciCommand::SetAdvertisingParameters) => {
                self.event_queue
                    .push_back(HciPacket::new(4, vec![0x0E, 0x04, 0x01, 0x06, 0x20, 0x00]));
            }
            Some(HciCommand::SetAdvertisingEnable) => {
                self.advertising = cmd.payload.len() > 2 && cmd.payload[2] != 0;
                self.event_queue
                    .push_back(HciPacket::new(4, vec![0x0E, 0x04, 0x01, 0x0A, 0x20, 0x00]));
            }
            Some(HciCommand::CreateConnection) | Some(HciCommand::Disconnect) => {
                // CommandStatus(Pending)
                let status_opcode = opcode.to_le_bytes();
                let mut resp = vec![0x0F, 0x04, 0x00, 0x01, 0x00];
                resp.extend_from_slice(&status_opcode);
                self.event_queue.push_back(HciPacket::new(4, resp));
            }
            Some(HciCommand::ReadLocalSupportedFeatures) => {
                self.event_queue.push_back(HciPacket::new(
                    4,
                    vec![
                        0x0E, 0x0C, 0x01, 0x03, 0x10, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x1F,
                        0x00, 0x00,
                    ],
                ));
            }
            None => {
                // Unknown command: CommandComplete with UnsupportedFeature (0x11)
                let cmd_opcode = opcode.to_le_bytes();
                let mut resp = vec![0x0E, 0x04, 0x01, 0x11];
                resp.extend_from_slice(&cmd_opcode);
                self.event_queue.push_back(HciPacket::new(4, resp));
            }
        }
    }

    /// Check if events or ACL data are pending for the host.
    pub fn has_pending(&self) -> bool {
        !self.event_queue.is_empty() || !self.acl_host_rx.is_empty()
    }

    /// Check if commands are queued for processing.
    pub fn has_commands(&self) -> bool {
        !self.cmd_queue.is_empty()
    }

    /// Count of pending host commands.
    pub fn cmd_pending_count(&self) -> usize {
        self.cmd_queue.len()
    }

    /// Count of pending events.
    pub fn event_pending_count(&self) -> usize {
        self.event_queue.len()
    }

    /// Register a receive callback (called when events/data arrive for host).
    pub fn on_recv(&mut self, cb: unsafe extern "C" fn()) {
        self.rx_callback = Some(cb);
    }

    /// Fire the rx callback if registered and data is pending.
    pub fn fire_rx_callback(&self) {
        if self.has_pending() {
            if let Some(cb) = self.rx_callback {
                unsafe {
                    cb();
                }
            }
        }
    }

    /// Drain ACL data from host (for inspection).
    pub fn drain_acl_tx(&mut self) -> Vec<HciPacket> {
        std::mem::take(&mut self.acl_host_tx).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hci_create() {
        let ctrl = VirtualHciController::new(0);
        assert_eq!(ctrl.id, 0);
        assert!(!ctrl.advertising);
        assert!(ctrl.connected_peer.is_none());
        assert!(!ctrl.has_pending());
    }

    #[test]
    fn test_hci_send_command_and_recv_event() {
        let mut ctrl = VirtualHciController::new(1);
        // Send HCI_Reset command (opcode 0x0C03, params=0)
        ctrl.send(1, &[0x03, 0x0C, 0x00]);
        assert!(ctrl.has_commands());
        assert_eq!(ctrl.process_commands(), 1);
        assert!(!ctrl.has_commands());
        assert!(ctrl.has_pending());
        // Read the response
        let mut buf = [0u8; 32];
        let n = ctrl.recv_into(&mut buf);
        assert!(n > 0);
        assert_eq!(buf[0], 4); // Event type
    }

    #[test]
    fn test_hci_advertising_enable() {
        let mut ctrl = VirtualHciController::new(2);
        // SetAdvertisingEnable(enable=1)
        ctrl.send(1, &[0x0A, 0x20, 0x01]);
        assert_eq!(ctrl.process_commands(), 1);
        assert!(ctrl.advertising);
    }

    #[test]
    fn test_hci_inject_event() {
        let mut ctrl = VirtualHciController::new(3);
        ctrl.inject_event(4, &[0x3E, 0x0C, 0x02, 0x01, 0x00]);
        assert!(ctrl.has_pending());
        let mut buf = [0u8; 32];
        let n = ctrl.recv_into(&mut buf);
        assert_eq!(n, 6); // 1 type byte + 5 payload bytes
        assert_eq!(buf[0], 4); // Event
        assert_eq!(buf[1], 0x3E);
    }

    #[test]
    fn test_hci_scripted_response() {
        let mut ctrl = VirtualHciController::new(4);
        // Script a custom response for opcode 0x0C03 (HCI_Reset)
        ctrl.script.insert(0x0C03, vec![0x99, 0x88, 0x77]);
        ctrl.send(1, &[0x03, 0x0C, 0x00]);
        assert_eq!(ctrl.process_commands(), 1);
        let mut buf = [0u8; 10];
        let n = ctrl.recv_into(&mut buf);
        assert_eq!(n, 4); // type + 3 payload bytes
        assert_eq!(buf[0], 4); // Event
        assert_eq!(buf[1..4], [0x99, 0x88, 0x77]);
    }

    #[test]
    fn test_hci_acl_data() {
        let mut ctrl = VirtualHciController::new(5);
        // Host sends ACL data
        ctrl.send(2, &[0x01, 0x02, 0x03]);
        let drained = ctrl.drain_acl_tx();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].packet_type, 2);
        assert_eq!(drained[0].payload, vec![0x01, 0x02, 0x03]);
    }

    #[test]
    fn test_hci_rx_callback() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static CALLED: AtomicBool = AtomicBool::new(false);
        extern "C" fn cb() {
            CALLED.store(true, Ordering::SeqCst);
        }

        let mut ctrl = VirtualHciController::new(6);
        ctrl.on_recv(cb);
        ctrl.fire_rx_callback(); // nothing pending — shouldn't fire
        assert!(!CALLED.load(Ordering::SeqCst));

        ctrl.inject_event(4, &[0x00]);
        ctrl.fire_rx_callback();
        assert!(CALLED.load(Ordering::SeqCst));
    }

    #[test]
    fn test_hci_unknown_command() {
        let mut ctrl = VirtualHciController::new(7);
        // Send an unknown command (opcode 0x0000)
        ctrl.send(1, &[0x00, 0x00, 0x00]);
        assert_eq!(ctrl.process_commands(), 1);
        let mut buf = [0u8; 32];
        let n = ctrl.recv_into(&mut buf);
        assert!(n > 0);
        assert_eq!(buf[0], 4); // Event type
                               // Response should be UnsupportedFeature (status=0x11 in payload)
    }

    #[test]
    fn test_hci_packet_type_enum() {
        assert_eq!(HciPacketType::from_u8(1), Some(HciPacketType::Command));
        assert_eq!(HciPacketType::from_u8(2), Some(HciPacketType::AclData));
        assert_eq!(HciPacketType::from_u8(4), Some(HciPacketType::Event));
        assert_eq!(HciPacketType::from_u8(99), None);
        assert_eq!(HciPacketType::Command.to_u8(), 1);
        assert_eq!(HciPacketType::Event.to_u8(), 4);
    }
}
