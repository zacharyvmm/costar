//! Virtual SPI controller (master mode).
//!
//! A `VirtualSpi` models a master-mode SPI bus with separate TX and RX
//! buffers.  Writes push data to the TX buffer; reads (full-duplex
//! transfers) consume bytes from the RX buffer that must be pre-loaded
//! via [`inject_rx`].  Chip-select state and SPI mode (CPOL/CPHA) are
//! tracked as metadata.
//!
//! The SPI controller is purely a data model — it does not schedule
//! events or raise interrupts directly.  Interrupt generation (transfer
//! complete, RX available) is handled by the caller or an adapter.

/// SPI mode (clock polarity / clock phase).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpiMode {
    /// CPOL=0, CPHA=0: clock idle low, sample on leading edge.
    Mode0,
    /// CPOL=0, CPHA=1: clock idle low, sample on trailing edge.
    Mode1,
    /// CPOL=1, CPHA=0: clock idle high, sample on leading edge.
    Mode2,
    /// CPOL=1, CPHA=1: clock idle high, sample on trailing edge.
    Mode3,
}

/// A virtual SPI controller (master mode).
#[derive(Debug, Clone)]
pub struct VirtualSpi {
    /// SPI controller ID.
    pub id: u32,
    /// Transmit buffer — data written by the master.
    pub tx_buf: Vec<u8>,
    /// Receive buffer — data received from the peripheral (full-duplex).
    pub rx_buf: Vec<u8>,
    /// Whether the controller is enabled.
    pub enabled: bool,
    /// SPI mode (CPOL/CPHA).
    pub mode: SpiMode,
    /// Clock frequency in Hz (metadata only).
    pub speed_hz: u32,
    /// Word size in bits (8 or 16 typically).
    pub word_size: u8,
    /// Whether chip select is active.
    pub cs_active: bool,
    /// Chip select polarity: true = active high, false = active low.
    pub cs_polarity_high: bool,
}

impl VirtualSpi {
    /// Create a new SPI controller with the given ID and clock speed.
    ///
    /// Defaults: Mode0, 8-bit word size, CS active low.
    pub fn new(id: u32, speed_hz: u32) -> Self {
        Self {
            id,
            tx_buf: Vec::new(),
            rx_buf: Vec::new(),
            enabled: true,
            mode: SpiMode::Mode0,
            speed_hz,
            word_size: 8,
            cs_active: false,
            cs_polarity_high: false,
        }
    }

    /// Set the SPI mode (CPOL/CPHA).
    pub fn set_mode(&mut self, mode: SpiMode) {
        self.mode = mode;
    }

    /// Set the word size in bits.  Panics if `bits` is not 8 or 16.
    pub fn set_word_size(&mut self, bits: u8) {
        assert!(bits == 8 || bits == 16, "word size must be 8 or 16 bits");
        self.word_size = bits;
    }

    /// Set the chip-select state.
    ///
    /// When `active` is true, CS is asserted (driven to the active level
    /// according to `cs_polarity_high`).  When false, CS is de-asserted.
    pub fn set_cs(&mut self, active: bool) {
        self.cs_active = active;
    }

    /// Enable or disable the SPI controller.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Full-duplex transfer: write `data` to the TX buffer and return
    /// bytes consumed from the RX buffer.
    ///
    /// The caller must pre-load RX data with [`inject_rx`] before
    /// calling this.  Returns at most `min(tx.len(), rx.len())` bytes.
    /// If the controller is disabled, returns an empty vector.
    pub fn transfer(&mut self, data: &[u8]) -> Vec<u8> {
        if !self.enabled {
            return Vec::new();
        }
        self.tx_buf.extend_from_slice(data);
        let n = data.len().min(self.rx_buf.len());
        self.rx_buf.drain(..n).collect()
    }

    /// Master-to-peripheral write only (ignores the RX buffer).
    ///
    /// Returns the number of bytes written.  If the controller is
    /// disabled, returns 0.
    pub fn write(&mut self, data: &[u8]) -> usize {
        if !self.enabled {
            return 0;
        }
        self.tx_buf.extend_from_slice(data);
        data.len()
    }

    /// Push bytes into the RX buffer (for test scripts to simulate
    /// peripheral responses).
    pub fn inject_rx(&mut self, data: &[u8]) {
        self.rx_buf.extend_from_slice(data);
    }

    /// Drain and return all TX bytes, leaving the TX buffer empty.
    pub fn drain_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx_buf)
    }

    /// Drain and return all RX bytes, leaving the RX buffer empty.
    pub fn drain_rx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.rx_buf)
    }

    /// Reset all buffers and flags to defaults.
    pub fn clear(&mut self) {
        self.tx_buf.clear();
        self.rx_buf.clear();
        self.enabled = true;
        self.mode = SpiMode::Mode0;
        self.word_size = 8;
        self.cs_active = false;
        self.cs_polarity_high = false;
    }

    /// Number of bytes waiting in the TX buffer.
    pub fn tx_len(&self) -> usize {
        self.tx_buf.len()
    }

    /// Number of bytes available in the RX buffer.
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
    fn test_new_defaults() {
        let spi = VirtualSpi::new(1, 8_000_000);
        assert_eq!(spi.id, 1);
        assert_eq!(spi.speed_hz, 8_000_000);
        assert!(spi.enabled);
        assert_eq!(spi.mode, SpiMode::Mode0);
        assert_eq!(spi.word_size, 8);
        assert!(!spi.cs_active);
        assert!(!spi.cs_polarity_high);
        assert!(spi.tx_buf.is_empty());
        assert!(spi.rx_buf.is_empty());
    }

    #[test]
    fn test_write_only() {
        let mut spi = VirtualSpi::new(0, 1_000_000);
        let n = spi.write(b"hello");
        assert_eq!(n, 5);
        assert_eq!(spi.tx_len(), 5);
        assert_eq!(spi.drain_tx(), b"hello");
        assert_eq!(spi.tx_len(), 0);
    }

    #[test]
    fn test_full_duplex_transfer() {
        let mut spi = VirtualSpi::new(0, 1_000_000);
        // Pre-load peripheral response
        spi.inject_rx(&[0xAA, 0xBB, 0xCC, 0xDD]);

        let rx = spi.transfer(&[0x01, 0x02, 0x03]);
        // TX gets all 3 bytes
        assert_eq!(spi.tx_len(), 3);
        // RX returns min(3, 4) = 3 bytes
        assert_eq!(rx, vec![0xAA, 0xBB, 0xCC]);
        // 1 byte remains in RX
        assert_eq!(spi.rx_len(), 1);
        assert_eq!(spi.drain_rx(), vec![0xDD]);

        let tx = spi.drain_tx();
        assert_eq!(tx, vec![0x01, 0x02, 0x03]);
    }

    #[test]
    fn test_disabled_controller_ignores_transfer() {
        let mut spi = VirtualSpi::new(0, 1_000_000);
        spi.set_enabled(false);

        spi.inject_rx(&[0x10, 0x20]);
        let rx = spi.transfer(&[0x01]);
        assert!(rx.is_empty());
        assert_eq!(spi.tx_len(), 0);

        let n = spi.write(&[0x01]);
        assert_eq!(n, 0);
        assert_eq!(spi.tx_len(), 0);
    }

    #[test]
    fn test_spi_mode_configuration() {
        let mut spi = VirtualSpi::new(0, 1_000_000);

        spi.set_mode(SpiMode::Mode3);
        assert_eq!(spi.mode, SpiMode::Mode3);

        spi.set_mode(SpiMode::Mode1);
        assert_eq!(spi.mode, SpiMode::Mode1);

        spi.set_mode(SpiMode::Mode2);
        assert_eq!(spi.mode, SpiMode::Mode2);

        spi.set_mode(SpiMode::Mode0);
        assert_eq!(spi.mode, SpiMode::Mode0);
    }

    #[test]
    fn test_cs_active_inactive() {
        let mut spi = VirtualSpi::new(0, 1_000_000);
        assert!(!spi.cs_active);

        spi.set_cs(true);
        assert!(spi.cs_active);

        spi.set_cs(false);
        assert!(!spi.cs_active);
    }

    #[test]
    fn test_clear_resets_all() {
        let mut spi = VirtualSpi::new(0, 1_000_000);

        // Modify everything from defaults
        spi.write(b"txdata");
        spi.inject_rx(b"rxdata");
        spi.set_enabled(false);
        spi.set_mode(SpiMode::Mode3);
        spi.set_word_size(16);
        spi.set_cs(true);
        spi.cs_polarity_high = true;

        spi.clear();

        assert!(spi.tx_buf.is_empty());
        assert!(spi.rx_buf.is_empty());
        assert!(spi.enabled);
        assert_eq!(spi.mode, SpiMode::Mode0);
        assert_eq!(spi.word_size, 8);
        assert!(!spi.cs_active);
        assert!(!spi.cs_polarity_high);
    }

    #[test]
    fn test_word_size_configuration() {
        let mut spi = VirtualSpi::new(0, 1_000_000);
        assert_eq!(spi.word_size, 8);

        spi.set_word_size(16);
        assert_eq!(spi.word_size, 16);

        spi.set_word_size(8);
        assert_eq!(spi.word_size, 8);
    }

    #[test]
    #[should_panic(expected = "word size must be 8 or 16 bits")]
    fn test_invalid_word_size_panics() {
        let mut spi = VirtualSpi::new(0, 1_000_000);
        spi.set_word_size(10);
    }

    #[test]
    fn test_transfer_more_tx_than_rx() {
        let mut spi = VirtualSpi::new(0, 1_000_000);
        // Pre-load only 1 byte
        spi.inject_rx(&[0x42]);

        let rx = spi.transfer(&[0x01, 0x02, 0x03]);
        // Only 1 byte available in RX
        assert_eq!(rx, vec![0x42]);
        assert_eq!(spi.rx_len(), 0);
        // All 3 TX bytes pushed
        assert_eq!(spi.tx_len(), 3);
    }

    #[test]
    fn test_cs_polarity_high() {
        let mut spi = VirtualSpi::new(0, 1_000_000);
        assert!(!spi.cs_polarity_high);

        // cs_active and cs_polarity_high are independent: the caller
        // interprets active-low/high meaning based on polarity.
        spi.cs_polarity_high = true;
        assert!(spi.cs_polarity_high);

        spi.set_cs(true);
        assert!(spi.cs_active);
        assert!(spi.cs_polarity_high);
    }
}
