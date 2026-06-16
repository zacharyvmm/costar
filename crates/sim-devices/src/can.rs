//! Virtual CAN bus controller.
//!
//! A `VirtualCan` models a CAN bus controller with TX/RX mailboxes (FIFO
//! queues), standard/extended ID support, data frames (0–8 bytes), remote
//! frames, loopback mode, and simplified error-state tracking.
//!
//! The CAN controller is purely a data model — it does not schedule
//! events or raise interrupts directly.  Interrupt generation (TX
//! complete, RX available, bus-off) is handled by the caller or an
//! adapter.

/// A single CAN frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanFrame {
    /// 11-bit or 29-bit identifier.
    pub id: u32,
    /// Whether this is an extended (29-bit) ID.
    pub is_extended: bool,
    /// Whether this is a remote transmission request (RTR) frame.
    pub is_remote: bool,
    /// Data length code: 0–8 bytes.
    pub dlc: u8,
    /// Frame data (only first `dlc` bytes are meaningful).
    pub data: [u8; 8],
}

impl CanFrame {
    /// Create a new data frame with standard (11-bit) ID.
    pub fn new_data(id: u32, data: &[u8]) -> Self {
        let dlc = (data.len().min(8)) as u8;
        let mut buf = [0u8; 8];
        buf[..dlc as usize].copy_from_slice(&data[..dlc as usize]);
        Self {
            id,
            is_extended: false,
            is_remote: false,
            dlc,
            data: buf,
        }
    }

    /// Create a new data frame with extended (29-bit) ID.
    pub fn new_data_ext(id: u32, data: &[u8]) -> Self {
        let mut f = Self::new_data(id, data);
        f.is_extended = true;
        f
    }

    /// Create a remote frame (RTR) — no data payload.
    pub fn new_remote(id: u32, is_extended: bool) -> Self {
        Self {
            id,
            is_extended,
            is_remote: true,
            dlc: 0,
            data: [0u8; 8],
        }
    }
}

/// CAN error states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanErrorState {
    /// Normal operation; no errors.
    ErrorActive,
    /// Warning level reached (TEC or REC ≥ 96).
    ErrorWarning,
    /// Error-passive: transmit only recessive bits (TEC or REC ≥ 128).
    ErrorPassive,
    /// Bus-off: controller disconnected from bus (TEC ≥ 256).
    BusOff,
}

impl CanErrorState {
    /// Determine the error state from the transmit and receive error counters.
    pub fn from_counters(tec: u16, rec: u16) -> Self {
        if tec >= 256 {
            Self::BusOff
        } else if tec >= 128 || rec >= 128 {
            Self::ErrorPassive
        } else if tec >= 96 || rec >= 96 {
            Self::ErrorWarning
        } else {
            Self::ErrorActive
        }
    }
}

/// A virtual CAN bus controller.
#[derive(Debug, Clone)]
pub struct VirtualCan {
    /// Controller ID.
    pub id: u32,
    /// Transmit mailbox (FIFO).  Frames pushed by `send()`.
    pub tx_queue: Vec<CanFrame>,
    /// Receive mailbox (FIFO).  Frames pushed by `inject_rx()`.
    pub rx_queue: Vec<CanFrame>,
    /// Whether the controller is enabled.
    pub enabled: bool,
    /// Loopback mode: TX frames are automatically copied to the RX queue.
    pub loopback: bool,
    /// Maximum number of frames in each queue.
    pub max_queue_len: usize,
    /// CAN bitrate in bits per second (metadata only).
    pub bitrate: u32,
    /// Transmit error counter (0–255, incremented on TX errors).
    pub tec: u16,
    /// Receive error counter (0–255, incremented on RX errors).
    pub rec: u16,
}

impl VirtualCan {
    /// Create a new CAN controller with the given ID and bitrate.
    ///
    /// Defaults to enabled, no loopback, 32-frame queues, 500 kbit/s.
    pub fn new(id: u32, bitrate: u32) -> Self {
        Self {
            id,
            tx_queue: Vec::new(),
            rx_queue: Vec::new(),
            enabled: true,
            loopback: false,
            max_queue_len: 32,
            bitrate,
            tec: 0,
            rec: 0,
        }
    }

    /// Send a CAN frame.
    ///
    /// Pushes the frame onto the TX queue.  If loopback is enabled,
    /// the frame is also copied to the RX queue.  Fails silently if
    /// the controller is not enabled or the TX queue is full.
    ///
    /// Returns `true` if the frame was actually enqueued.
    pub fn send(&mut self, frame: CanFrame) -> bool {
        if !self.enabled || self.error_state() == CanErrorState::BusOff {
            self.tx_error();
            return false;
        }
        if self.tx_queue.len() >= self.max_queue_len {
            self.tx_error();
            return false;
        }
        self.tx_queue.push(frame.clone());
        if self.loopback {
            // Loopback: frame immediately appears in RX queue.
            if self.rx_queue.len() < self.max_queue_len {
                self.rx_queue.push(frame);
            }
        }
        true
    }

    /// Receive the oldest frame from the RX queue.
    ///
    /// Returns `None` if the RX queue is empty or the controller is
    /// not enabled.
    pub fn recv(&mut self) -> Option<CanFrame> {
        if !self.enabled {
            return None;
        }
        if self.rx_queue.is_empty() {
            return None;
        }
        Some(self.rx_queue.remove(0))
    }

    /// Inject a frame into the RX queue (simulates external node sending).
    pub fn inject_rx(&mut self, frame: CanFrame) {
        if !self.enabled {
            return;
        }
        if self.rx_queue.len() >= self.max_queue_len {
            // FIFO overrun — increment REC.
            self.rec = self.rec.saturating_add(1);
            return;
        }
        self.rx_queue.push(frame);
    }

    /// Get the current error state.
    pub fn error_state(&self) -> CanErrorState {
        CanErrorState::from_counters(self.tec, self.rec)
    }

    /// Increment the transmit error counter (decremented on success).
    fn tx_error(&mut self) {
        self.tec = self.tec.saturating_add(8);
    }

    /// Reset error counters, clear queues, and restore BusOff.
    pub fn reset(&mut self) {
        self.tx_queue.clear();
        self.rx_queue.clear();
        self.tec = 0;
        self.rec = 0;
    }

    /// Number of frames in the TX queue.
    pub fn tx_pending(&self) -> usize {
        self.tx_queue.len()
    }

    /// Number of frames in the RX queue.
    pub fn rx_pending(&self) -> usize {
        self.rx_queue.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_defaults() {
        let can = VirtualCan::new(0, 500_000);
        assert_eq!(can.id, 0);
        assert_eq!(can.bitrate, 500_000);
        assert!(can.enabled);
        assert!(!can.loopback);
        assert_eq!(can.max_queue_len, 32);
        assert_eq!(can.tec, 0);
        assert_eq!(can.rec, 0);
        assert_eq!(can.tx_pending(), 0);
        assert_eq!(can.rx_pending(), 0);
        assert_eq!(can.error_state(), CanErrorState::ErrorActive);
    }

    #[test]
    fn test_send_and_recv_normal() {
        let mut can = VirtualCan::new(1, 1_000_000);

        let frame = CanFrame::new_data(0x123, &[0xAA, 0xBB]);
        assert!(can.send(frame.clone()));
        assert_eq!(can.tx_pending(), 1);
        assert_eq!(can.rx_pending(), 0); // no loopback

        // Normal send does NOT place frame in RX queue
        assert!(can.recv().is_none());
    }

    #[test]
    fn test_loopback_mode() {
        let mut can = VirtualCan::new(2, 500_000);
        can.loopback = true;

        let frame = CanFrame::new_data(0x200, &[0x01, 0x02, 0x03]);
        assert!(can.send(frame.clone()));

        // Frame should be in both queues
        assert_eq!(can.tx_pending(), 1);
        assert_eq!(can.rx_pending(), 1);

        // Receive it
        let rx = can.recv().unwrap();
        assert_eq!(rx.id, 0x200);
        assert_eq!(rx.dlc, 3);
        assert_eq!(&rx.data[..3], &[0x01, 0x02, 0x03]);

        // RX queue is now empty
        assert_eq!(can.rx_pending(), 0);
    }

    #[test]
    fn test_inject_rx_external() {
        let mut can = VirtualCan::new(3, 250_000);

        let frame = CanFrame::new_data_ext(0x1ABCDEF, &[0x42]);
        can.inject_rx(frame);

        assert_eq!(can.rx_pending(), 1);

        let rx = can.recv().unwrap();
        assert!(rx.is_extended);
        assert_eq!(rx.id, 0x1ABCDEF);
        assert_eq!(rx.dlc, 1);
        assert_eq!(rx.data[0], 0x42);
    }

    #[test]
    fn test_remote_frame() {
        let mut can = VirtualCan::new(4, 500_000);
        can.loopback = true;

        let frame = CanFrame::new_remote(0x7FF, true);
        assert!(can.send(frame));
        assert_eq!(can.rx_pending(), 1);

        let rx = can.recv().unwrap();
        assert!(rx.is_remote);
        assert!(rx.is_extended);
        assert_eq!(rx.id, 0x7FF);
        assert_eq!(rx.dlc, 0);
    }

    #[test]
    fn test_disabled_controller() {
        let mut can = VirtualCan::new(5, 500_000);
        can.enabled = false;

        assert!(!can.send(CanFrame::new_data(0x100, &[0x00])));
        assert!(can.recv().is_none());

        // inject_rx is also ignored when disabled
        can.inject_rx(CanFrame::new_data(0x200, &[0x01]));
        assert_eq!(can.rx_pending(), 0);
    }

    #[test]
    fn test_error_counters_and_state() {
        let mut can = VirtualCan::new(6, 500_000);

        assert_eq!(can.error_state(), CanErrorState::ErrorActive);

        // Artificially bump TEC to warning level
        can.tec = 100;
        assert_eq!(can.error_state(), CanErrorState::ErrorWarning);

        // Bump to error-passive
        can.tec = 130;
        assert_eq!(can.error_state(), CanErrorState::ErrorPassive);

        // Bump to bus-off
        can.tec = 256;
        assert_eq!(can.error_state(), CanErrorState::BusOff);

        // Bus-off controller cannot send
        assert!(!can.send(CanFrame::new_data(0x100, &[0x01])));
        assert!(can.tec > 256); // TX error increments counter further
    }

    #[test]
    fn test_queue_overflow() {
        let mut can = VirtualCan::new(7, 500_000);
        can.loopback = true;
        // Use a small queue to test overflow
        can.max_queue_len = 3;

        // Fill TX queue (and loopback → RX queue gets copies too)
        for i in 0..3 {
            assert!(can.send(CanFrame::new_data(0x100 + i as u32, &[i])));
        }
        assert_eq!(can.tx_pending(), 3);
        assert_eq!(can.rx_pending(), 3);

        // Next send should fail (TX queue full)
        assert!(!can.send(CanFrame::new_data(0x103, &[0x04])));

        // TEC should have been incremented
        assert!(can.tec > 0);

        // RX overflow from inject_rx
        can.inject_rx(CanFrame::new_data(0x200, &[0xFF]));
        assert_eq!(can.rx_pending(), 3); // still full
        assert!(can.rec > 0); // REC incremented due to overflow
    }

    #[test]
    fn test_reset_clears_all() {
        let mut can = VirtualCan::new(8, 500_000);
        can.loopback = true;

        can.send(CanFrame::new_data(0x100, &[0x01, 0x02]));
        can.tec = 50;
        can.rec = 25;

        can.reset();

        assert_eq!(can.tx_pending(), 0);
        assert_eq!(can.rx_pending(), 0);
        assert_eq!(can.tec, 0);
        assert_eq!(can.rec, 0);
        assert_eq!(can.error_state(), CanErrorState::ErrorActive);
        // loopback and enabled are preserved
        assert!(can.loopback);
        assert!(can.enabled);
    }
}
