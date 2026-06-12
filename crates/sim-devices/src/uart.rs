//! Virtual UART peripheral.
//!
//! A `VirtualUart` models a UART with separate TX and RX buffers.
//! Writes go to the trace; reads return bytes from the RX buffer
//! (which can be pre-filled by test scripts or host-connected adapters).
//!
//! The UART is purely a data model — it does not schedule events or
//! raise interrupts directly.  Interrupt generation (TX complete, RX
//! available) is handled by the caller or an adapter.

/// A virtual UART device.
#[derive(Debug, Clone)]
pub struct VirtualUart {
    /// UART device ID.
    pub id: u32,
    /// Transmit buffer (what the guest has written out).
    pub tx_buf: Vec<u8>,
    /// Receive buffer (what is available for the guest to read).
    pub rx_buf: Vec<u8>,
    /// Baud rate (for metadata only — the simulator is instant).
    pub baud_rate: u32,
    /// Whether the UART is enabled.
    pub enabled: bool,
    /// Last value written (for polling-style tests).
    pub last_tx: Option<u8>,
}

impl VirtualUart {
    /// Create a new UART with the given ID and baud rate.
    pub fn new(id: u32, baud_rate: u32) -> Self {
        Self {
            id,
            tx_buf: Vec::new(),
            rx_buf: Vec::new(),
            baud_rate,
            enabled: true,
            last_tx: None,
        }
    }

    /// Write a byte to the TX buffer (from the guest firmware).
    pub fn write_byte(&mut self, byte: u8) {
        if self.enabled {
            self.tx_buf.push(byte);
            self.last_tx = Some(byte);
        }
    }

    /// Write multiple bytes to the TX buffer.
    pub fn write(&mut self, data: &[u8]) -> usize {
        if !self.enabled {
            return 0;
        }
        self.tx_buf.extend_from_slice(data);
        if let Some(&last) = data.last() {
            self.last_tx = Some(last);
        }
        data.len()
    }

    /// Read a byte from the RX buffer.  Returns `None` if empty.
    pub fn read_byte(&mut self) -> Option<u8> {
        if self.rx_buf.is_empty() {
            None
        } else {
            Some(self.rx_buf.remove(0))
        }
    }

    /// Read up to `len` bytes from the RX buffer.
    pub fn read(&mut self, len: usize) -> Vec<u8> {
        let actual = len.min(self.rx_buf.len());
        self.rx_buf.drain(..actual).collect()
    }

    /// Push bytes into the RX buffer (for test scripts / adapters).
    pub fn push_rx(&mut self, data: &[u8]) {
        self.rx_buf.extend_from_slice(data);
    }

    /// How many bytes are available in the TX buffer.
    pub fn tx_len(&self) -> usize {
        self.tx_buf.len()
    }

    /// How many bytes are available in the RX buffer.
    pub fn rx_len(&self) -> usize {
        self.rx_buf.len()
    }

    /// Drain and return all TX bytes (for trace capture).
    pub fn drain_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx_buf)
    }

    /// Clear both buffers.
    pub fn clear(&mut self) {
        self.tx_buf.clear();
        self.rx_buf.clear();
        self.last_tx = None;
    }

    /// Enable or disable the UART.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uart_write_read() {
        let mut uart = VirtualUart::new(0, 115200);

        // Write from guest
        uart.write_byte(b'H');
        uart.write_byte(b'i');
        assert_eq!(uart.tx_len(), 2);
        assert_eq!(uart.last_tx, Some(b'i'));

        // Drain for trace
        let tx_data = uart.drain_tx();
        assert_eq!(tx_data, vec![b'H', b'i']);
        assert_eq!(uart.tx_len(), 0);
    }

    #[test]
    fn test_uart_rx_buffer() {
        let mut uart = VirtualUart::new(1, 9600);

        // Simulator pushes data into RX buffer (simulates incoming data)
        uart.push_rx(b"hello");
        assert_eq!(uart.rx_len(), 5);

        // Guest reads
        assert_eq!(uart.read_byte(), Some(b'h'));
        assert_eq!(uart.read_byte(), Some(b'e'));
        assert_eq!(uart.read(2), vec![b'l', b'l']);
        assert_eq!(uart.read_byte(), Some(b'o'));
        assert!(uart.read_byte().is_none());
    }

    #[test]
    fn test_uart_disabled_ignores_write() {
        let mut uart = VirtualUart::new(2, 115200);
        uart.set_enabled(false);
        assert_eq!(uart.write(b"test"), 0);
        assert_eq!(uart.tx_len(), 0);
    }

    #[test]
    fn test_clear_resets_buffers() {
        let mut uart = VirtualUart::new(3, 115200);
        uart.write_byte(b'x');
        uart.push_rx(b"y");
        uart.clear();
        assert_eq!(uart.tx_len(), 0);
        assert_eq!(uart.rx_len(), 0);
        assert!(uart.last_tx.is_none());
    }
}
