//! Virtual Ethernet device for guest networking stacks.
//!
#![allow(missing_docs)]
//! Connects Zephyr's net_if or FreeRTOS+TCP's NetworkInterface to
//! costar's deterministic smoltcp stack (or host sockets in interactive mode).

use std::collections::VecDeque;

/// A deterministic virtual Ethernet device with separate rx/tx FIFO queues.
pub struct VirtualEthDevice {
    pub id: u32,
    pub mac: [u8; 6],
    pub mtu: usize,
    rx_queue: VecDeque<Vec<u8>>,
    tx_queue: VecDeque<Vec<u8>>,
    pub rx_callback: Option<unsafe extern "C" fn()>,
}

impl VirtualEthDevice {
    pub fn new(id: u32, mac: [u8; 6], mtu: usize) -> Self {
        Self {
            id,
            mac,
            mtu,
            rx_queue: VecDeque::new(),
            tx_queue: VecDeque::new(),
            rx_callback: None,
        }
    }

    /// Guest sends a frame → pushed to rx_queue (guest-side rx = our injection side).
    /// Returns number of bytes queued.
    pub fn send(&mut self, data: &[u8]) -> usize {
        let len = data.len();
        self.rx_queue.push_back(data.to_vec());
        len
    }

    /// Guest receives a frame from tx_queue → copied into buf.
    /// Returns number of bytes written, or 0 if empty.
    pub fn recv_into(&mut self, buf: &mut [u8]) -> usize {
        match self.tx_queue.pop_front() {
            Some(frame) => {
                let len = frame.len().min(buf.len());
                buf[..len].copy_from_slice(&frame[..len]);
                len
            }
            None => 0,
        }
    }

    /// Inject a frame from host/test script into the guest (via tx_queue).
    pub fn inject_rx(&mut self, frame: Vec<u8>) {
        self.tx_queue.push_back(frame);
    }

    /// Drain guest-sent frames (rx_queue) for capture/inspection.
    pub fn drain_tx(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.rx_queue).into()
    }

    /// Check if any frames are pending for the guest.
    pub fn has_rx(&self) -> bool {
        !self.tx_queue.is_empty()
    }

    /// Check if any guest-sent frames are pending for drain.
    pub fn has_tx(&self) -> bool {
        !self.rx_queue.is_empty()
    }

    /// Register a receive callback (called when frames arrive for the guest).
    pub fn on_recv(&mut self, cb: unsafe extern "C" fn()) {
        self.rx_callback = Some(cb);
    }

    /// Fire the rx callback if one is registered and frames are pending.
    pub fn fire_rx_callback(&self) {
        if self.has_rx() {
            if let Some(cb) = self.rx_callback {
                unsafe {
                    cb();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eth_create() {
        let dev = VirtualEthDevice::new(0, [0x02, 0x00, 0x00, 0x00, 0x00, 0x01], 1500);
        assert_eq!(dev.id, 0);
        assert_eq!(dev.mac, [0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(dev.mtu, 1500);
        assert!(!dev.has_rx());
        assert!(!dev.has_tx());
    }

    #[test]
    fn test_eth_send_and_recv() {
        let mut dev = VirtualEthDevice::new(0, [0; 6], 1500);
        // Guest sends a frame -> rx_queue
        let frame = b"hello ethernet frame";
        assert_eq!(dev.send(frame), frame.len());
        assert!(dev.has_tx());
        // Drain the guest-sent frame
        let drained = dev.drain_tx();
        assert_eq!(drained.len(), 1);
        assert_eq!(&drained[0], frame);
    }

    #[test]
    fn test_eth_inject_and_recv() {
        let mut dev = VirtualEthDevice::new(0, [0; 6], 1500);
        // Inject a frame from host -> tx_queue
        let frame = vec![0x00; 64];
        dev.inject_rx(frame.clone());
        assert!(dev.has_rx());
        // Guest reads it back
        let mut buf = [0u8; 128];
        let n = dev.recv_into(&mut buf);
        assert_eq!(n, 64);
        assert_eq!(&buf[..64], &frame[..]);
        assert!(!dev.has_rx());
    }

    #[test]
    fn test_eth_recv_empty() {
        let mut dev = VirtualEthDevice::new(0, [0; 6], 1500);
        let mut buf = [0u8; 64];
        assert_eq!(dev.recv_into(&mut buf), 0);
    }

    #[test]
    fn test_eth_recv_partial_buf() {
        let mut dev = VirtualEthDevice::new(0, [0; 6], 1500);
        let frame = vec![0xAA; 100];
        dev.inject_rx(frame);
        let mut buf = [0u8; 50];
        let n = dev.recv_into(&mut buf);
        assert_eq!(n, 50);
        assert_eq!(&buf[..], &[0xAA; 50]);
    }

    #[test]
    fn test_eth_rx_callback() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static CALLED: AtomicBool = AtomicBool::new(false);
        extern "C" fn cb() {
            CALLED.store(true, Ordering::SeqCst);
        }

        let mut dev = VirtualEthDevice::new(0, [0; 6], 1500);
        dev.on_recv(cb);
        // No frames yet, callback should not fire
        dev.fire_rx_callback();
        assert!(!CALLED.load(Ordering::SeqCst));
        // Inject a frame, now callback fires
        dev.inject_rx(vec![0; 10]);
        dev.fire_rx_callback();
        assert!(CALLED.load(Ordering::SeqCst));
    }

    #[test]
    fn test_eth_multiple_frames() {
        let mut dev = VirtualEthDevice::new(0, [0; 6], 1500);
        dev.inject_rx(vec![1]);
        dev.inject_rx(vec![2, 2]);
        dev.inject_rx(vec![3, 3, 3]);
        assert!(dev.has_rx());
        let mut buf = [0u8; 10];
        assert_eq!(dev.recv_into(&mut buf), 1);
        assert_eq!(dev.recv_into(&mut buf), 2);
        assert_eq!(dev.recv_into(&mut buf), 3);
        assert!(!dev.has_rx());
    }
}
