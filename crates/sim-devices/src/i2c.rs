//! Virtual I2C controller (master mode).
//!
//! A `VirtualI2c` models an I2C bus controller in master mode with
//! separate TX and RX buffers.  Writes push bytes onto the bus;
//! reads drain bytes from the RX buffer (which can be pre-filled by
//! test scripts via [`inject_rx`]).
//!
//! The I2C controller is purely a data model — it does not schedule
//! events or raise interrupts directly.  Interrupt generation (TX
//! complete, RX available, NACK) is handled by the caller or an
//! adapter.

/// A virtual I2C controller (master mode).
#[derive(Debug, Clone)]
pub struct VirtualI2c {
    /// I2C controller ID.
    pub id: u32,
    /// Current target address (set before read/write).
    pub address: Option<u16>,
    /// Transmit buffer — data written by the master.
    pub tx_buf: Vec<u8>,
    /// Receive buffer — data read from the target.
    pub rx_buf: Vec<u8>,
    /// Whether the controller is enabled.
    pub enabled: bool,
    /// Whether a NACK was received on the last operation.
    pub nack: bool,
    /// Whether the bus is busy (transfer in progress).
    pub busy: bool,
    /// I2C speed in Hz (metadata only).
    pub speed_hz: u32,
    /// Whether to use 10-bit addressing (false = 7-bit).
    pub ten_bit: bool,
}

impl VirtualI2c {
    /// Create a new I2C controller with the given ID and speed.
    ///
    /// Defaults to enabled, 7-bit addressing, not busy, no NACK,
    /// at the requested speed (commonly 100_000 or 400_000).
    pub fn new(id: u32, speed_hz: u32) -> Self {
        Self {
            id,
            address: None,
            tx_buf: Vec::new(),
            rx_buf: Vec::new(),
            enabled: true,
            nack: false,
            busy: false,
            speed_hz,
            ten_bit: false,
        }
    }

    /// Set the target address for subsequent reads and writes.
    ///
    /// `ten_bit` selects 10-bit addressing; `false` selects 7-bit.
    pub fn set_address(&mut self, addr: u16, ten_bit: bool) {
        self.address = Some(addr);
        self.ten_bit = ten_bit;
    }

    /// Write data from the master to the target.
    ///
    /// Bytes are pushed onto the TX buffer.  If the controller is
    /// not enabled, nothing is written.  Returns the number of bytes
    /// actually written.
    pub fn write(&mut self, data: &[u8]) -> usize {
        if !self.enabled {
            return 0;
        }
        self.tx_buf.extend_from_slice(data);
        self.busy = true;
        data.len()
    }

    /// Read up to `len` bytes from the target (drain from RX buffer).
    ///
    /// Returns a `Vec<u8>` with the bytes actually read (may be
    /// shorter than `len` if the buffer is exhausted).
    pub fn read(&mut self, len: usize) -> Vec<u8> {
        let actual = len.min(self.rx_buf.len());
        let data: Vec<u8> = self.rx_buf.drain(..actual).collect();
        if !data.is_empty() {
            self.busy = true;
        }
        data
    }

    /// Combined write-then-read operation (like an I2C repeated start).
    ///
    /// First writes `tx_data` to the TX buffer, then reads up to
    /// `rx_len` bytes from the RX buffer.  Returns a tuple of
    /// `(bytes_written, bytes_read)`.
    pub fn write_read(&mut self, tx_data: &[u8], rx_len: usize) -> (usize, Vec<u8>) {
        let written = self.write(tx_data);
        let read = self.read(rx_len);
        (written, read)
    }

    /// Inject bytes into the RX buffer (for test scripts or adapters
    /// that simulate the peripheral's response).
    pub fn inject_rx(&mut self, data: &[u8]) {
        self.rx_buf.extend_from_slice(data);
    }

    /// Set or clear the NACK flag.
    pub fn set_nack(&mut self, nack: bool) {
        self.nack = nack;
    }

    /// Drain and return all bytes from the TX buffer.
    pub fn drain_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx_buf)
    }

    /// Reset all buffers, flags, and address to defaults.
    ///
    /// After `clear()` the controller is still enabled and retains
    /// its ID and speed.
    pub fn clear(&mut self) {
        self.tx_buf.clear();
        self.rx_buf.clear();
        self.address = None;
        self.nack = false;
        self.busy = false;
        self.ten_bit = false;
    }

    /// Number of bytes currently in the TX buffer.
    pub fn tx_len(&self) -> usize {
        self.tx_buf.len()
    }

    /// Number of bytes currently in the RX buffer.
    pub fn rx_len(&self) -> usize {
        self.rx_buf.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_basic_fields() {
        let i2c = VirtualI2c::new(0, 100_000);
        assert_eq!(i2c.id, 0);
        assert_eq!(i2c.speed_hz, 100_000);
        assert!(i2c.enabled);
        assert!(!i2c.nack);
        assert!(!i2c.busy);
        assert!(!i2c.ten_bit);
        assert!(i2c.address.is_none());
        assert_eq!(i2c.tx_len(), 0);
        assert_eq!(i2c.rx_len(), 0);
    }

    #[test]
    fn test_write_to_tx_buffer() {
        let mut i2c = VirtualI2c::new(1, 400_000);
        i2c.set_address(0x50, false);

        let n = i2c.write(b"hello");
        assert_eq!(n, 5);
        assert_eq!(i2c.tx_len(), 5);
        assert!(i2c.busy);

        let n2 = i2c.write(b" world");
        assert_eq!(n2, 6);
        assert_eq!(i2c.tx_len(), 11);
    }

    #[test]
    fn test_read_from_rx_buffer() {
        let mut i2c = VirtualI2c::new(2, 100_000);

        // Pre-fill the RX buffer (simulates peripheral response)
        i2c.inject_rx(&[0xAA, 0xBB, 0xCC, 0xDD]);

        let data = i2c.read(3);
        assert_eq!(data, vec![0xAA, 0xBB, 0xCC]);
        assert_eq!(i2c.rx_len(), 1);
        assert!(i2c.busy);

        // Read the remaining byte
        let data2 = i2c.read(2);
        assert_eq!(data2, vec![0xDD]);
        assert_eq!(i2c.rx_len(), 0);

        // Reading from empty buffer returns empty vec
        let data3 = i2c.read(5);
        assert!(data3.is_empty());
    }

    #[test]
    fn test_combined_write_read() {
        let mut i2c = VirtualI2c::new(3, 100_000);
        i2c.set_address(0x68, false);

        // Inject expected response for the read portion
        i2c.inject_rx(&[0x12, 0x34]);

        let (written, read) = i2c.write_read(&[0x01, 0x02], 4);
        assert_eq!(written, 2);
        assert_eq!(read, vec![0x12, 0x34]);
        assert_eq!(i2c.tx_len(), 2);
        assert_eq!(i2c.rx_len(), 0);
        assert!(i2c.busy);
    }

    #[test]
    fn test_nack_detection() {
        let mut i2c = VirtualI2c::new(4, 100_000);

        // Initially no NACK
        assert!(!i2c.nack);

        // Set NACK
        i2c.set_nack(true);
        assert!(i2c.nack);

        // Clear NACK
        i2c.set_nack(false);
        assert!(!i2c.nack);
    }

    #[test]
    fn test_disabled_controller_ignores_operations() {
        let mut i2c = VirtualI2c::new(5, 100_000);
        i2c.enabled = false;

        // Write should be ignored
        let n = i2c.write(b"data");
        assert_eq!(n, 0);
        assert_eq!(i2c.tx_len(), 0);

        // Inject RX still works (adapter can always push data)
        i2c.inject_rx(&[0x42]);
        assert_eq!(i2c.rx_len(), 1);

        // Read still works even when disabled (guest may still drain)
        let data = i2c.read(1);
        assert_eq!(data, vec![0x42]);
    }

    #[test]
    fn test_clear_resets_everything() {
        let mut i2c = VirtualI2c::new(6, 400_000);
        i2c.set_address(0x3C, false);
        i2c.write(b"tx");
        i2c.inject_rx(b"rx");
        i2c.set_nack(true);
        i2c.busy = true;

        i2c.clear();

        assert_eq!(i2c.tx_len(), 0);
        assert_eq!(i2c.rx_len(), 0);
        assert!(i2c.address.is_none());
        assert!(!i2c.nack);
        assert!(!i2c.busy);
        assert!(!i2c.ten_bit);
        // ID and speed are preserved
        assert_eq!(i2c.id, 6);
        assert_eq!(i2c.speed_hz, 400_000);
        assert!(i2c.enabled);
    }

    #[test]
    fn test_ten_bit_addressing_flag() {
        let mut i2c = VirtualI2c::new(7, 100_000);

        // Default is 7-bit
        assert!(!i2c.ten_bit);

        // Set 10-bit address
        i2c.set_address(0x3FF, true);
        assert_eq!(i2c.address, Some(0x3FF));
        assert!(i2c.ten_bit);

        // Switch back to 7-bit
        i2c.set_address(0x50, false);
        assert_eq!(i2c.address, Some(0x50));
        assert!(!i2c.ten_bit);
    }

    #[test]
    fn test_drain_tx() {
        let mut i2c = VirtualI2c::new(8, 100_000);

        i2c.write(b"drain-me");
        assert_eq!(i2c.tx_len(), 8);

        let drained = i2c.drain_tx();
        assert_eq!(drained, b"drain-me".to_vec());
        assert_eq!(i2c.tx_len(), 0);

        // Second drain on empty buffer returns empty vec
        let drained2 = i2c.drain_tx();
        assert!(drained2.is_empty());
    }
}
