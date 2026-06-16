//! Virtual non-volatile storage devices.
//!
//! This module provides two virtual storage models:
//! * [`VirtualEeprom`] — byte-addressable EEPROM with write-count tracking
//! * [`VirtualFlash`] — page-addressed flash memory with per-page erase counts
//!
//! Both devices are purely data models — they do not schedule events or
//! raise interrupts directly.  Interrupt generation (write complete,
//! error) is handled by the caller or an adapter.

/// Default EEPROM size in bytes.
pub const EEPROM_DEFAULT_SIZE: usize = 4096;

/// Default Flash page size in bytes.
pub const FLASH_DEFAULT_PAGE_SIZE: usize = 256;

/// Default Flash page count.
pub const FLASH_DEFAULT_PAGE_COUNT: usize = 64;

/// Erased byte value for flash memory (all bits set).
pub const FLASH_ERASED_VALUE: u8 = 0xFF;

// ---------------------------------------------------------------------------
// VirtualEeprom
// ---------------------------------------------------------------------------

/// A byte-addressable virtual EEPROM with write-count tracking.
///
/// The EEPROM models a simple non-volatile storage device.  Every byte
/// can be read and written independently.  A `write_count` counter
/// tracks the total number of byte writes (including overwrites of the
/// same location).
#[derive(Debug, Clone)]
pub struct VirtualEeprom {
    /// EEPROM device ID.
    pub id: u32,
    /// Total size in bytes.
    pub size: usize,
    /// Storage array.
    pub data: Vec<u8>,
    /// Total number of byte writes performed.
    pub write_count: u64,
}

impl VirtualEeprom {
    /// Create a new virtual EEPROM.
    ///
    /// The storage is initialised to zero.  Pass `size` to set a custom
    /// size; otherwise the default of 4 KB ([`EEPROM_DEFAULT_SIZE`]) is
    /// used.
    pub fn new(id: u32) -> Self {
        Self {
            id,
            size: EEPROM_DEFAULT_SIZE,
            data: vec![0u8; EEPROM_DEFAULT_SIZE],
            write_count: 0,
        }
    }

    /// Create a new virtual EEPROM with a custom size.
    pub fn with_size(id: u32, size: usize) -> Self {
        Self {
            id,
            size,
            data: vec![0u8; size],
            write_count: 0,
        }
    }

    /// Read a byte from the EEPROM.
    ///
    /// Returns `Some(byte)` if `addr` is within bounds, or `None` if
    /// `addr >= self.size`.
    pub fn read(&self, addr: usize) -> Option<u8> {
        if addr < self.size {
            Some(self.data[addr])
        } else {
            None
        }
    }

    /// Write a byte to the EEPROM.
    ///
    /// Returns `true` on success, `false` if `addr` is out of bounds.
    /// Increments `write_count` on every successful write.
    pub fn write(&mut self, addr: usize, byte: u8) -> bool {
        if addr < self.size {
            self.data[addr] = byte;
            self.write_count += 1;
            true
        } else {
            false
        }
    }

    /// Bulk-initialise the entire EEPROM with a pattern byte.
    ///
    /// Every addressable location is set to `pattern`.  The operation
    /// does **not** increment `write_count`.
    pub fn fill(&mut self, pattern: u8) {
        self.data.fill(pattern);
    }
}

// ---------------------------------------------------------------------------
// VirtualFlash
// ---------------------------------------------------------------------------

/// A page-addressed virtual flash memory with per-page erase counts.
///
/// Flash memory is organised into pages.  Writes only succeed to
/// locations that have been previously erased (contain `0xFF`).
/// Pages must be explicitly erased before new data can be written to
/// them.  A per-page `erase_count` tracks endurance.
#[derive(Debug, Clone)]
pub struct VirtualFlash {
    /// Flash device ID.
    pub id: u32,
    /// Page size in bytes (default 256).
    pub page_size: usize,
    /// Number of pages.
    pub page_count: usize,
    /// Storage array (size = page_size * page_count).
    pub data: Vec<u8>,
    /// Per-page erase counter.
    pub erase_count: Vec<u64>,
}

impl VirtualFlash {
    /// Create a new virtual flash device with defaults.
    ///
    /// Default: 64 pages of 256 bytes each (16 KB total), all bytes
    /// initialised to the erased value (`0xFF`).
    pub fn new(id: u32) -> Self {
        let page_size = FLASH_DEFAULT_PAGE_SIZE;
        let page_count = FLASH_DEFAULT_PAGE_COUNT;
        Self {
            id,
            page_size,
            page_count,
            data: vec![FLASH_ERASED_VALUE; page_size * page_count],
            erase_count: vec![0u64; page_count],
        }
    }

    /// Create a new virtual flash device with custom geometry.
    ///
    /// All bytes are initialised to the erased value (`0xFF`).
    pub fn with_geometry(id: u32, page_size: usize, page_count: usize) -> Self {
        Self {
            id,
            page_size,
            page_count,
            data: vec![FLASH_ERASED_VALUE; page_size * page_count],
            erase_count: vec![0u64; page_count],
        }
    }

    /// Total capacity in bytes.
    pub fn total_size(&self) -> usize {
        self.page_size * self.page_count
    }

    /// Read a byte from flash by absolute address.
    ///
    /// Returns `Some(byte)` if `addr` is within bounds, or `None`
    /// otherwise.
    pub fn read(&self, addr: usize) -> Option<u8> {
        if addr < self.total_size() {
            Some(self.data[addr])
        } else {
            None
        }
    }

    /// Write data into a page at a given byte offset.
    ///
    /// # Constraints
    ///
    /// * `page` must be less than `page_count`.
    /// * `offset + data.len()` must be ≤ `page_size`.
    /// * Every target byte must currently be in the erased state
    ///   (`0xFF`), otherwise the entire operation fails with no
    ///   partial writes.
    ///
    /// Returns `true` on success, `false` if any constraint is
    /// violated.
    pub fn write_page(&mut self, page: usize, offset: usize, data: &[u8]) -> bool {
        if page >= self.page_count {
            return false;
        }
        if offset + data.len() > self.page_size {
            return false;
        }

        let base = page * self.page_size + offset;

        // Check: all target bytes must be erased (0xFF).
        for (i, _) in data.iter().enumerate() {
            if self.data[base + i] != FLASH_ERASED_VALUE {
                return false;
            }
        }

        // Perform the write.
        for (i, &byte) in data.iter().enumerate() {
            self.data[base + i] = byte;
        }

        true
    }

    /// Erase a page (fill with erased value `0xFF`).
    ///
    /// Increments the per-page `erase_count`.  Returns `true` on
    /// success, `false` if `page` is out of bounds.
    pub fn erase_page(&mut self, page: usize) -> bool {
        if page >= self.page_count {
            return false;
        }
        let start = page * self.page_size;
        let end = start + self.page_size;
        self.data[start..end].fill(FLASH_ERASED_VALUE);
        self.erase_count[page] += 1;
        true
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── EEPROM tests ────────────────────────────────────────────────────

    #[test]
    fn test_eeprom_read_write() {
        let mut eeprom = VirtualEeprom::new(1);
        assert_eq!(eeprom.read(0), Some(0));
        assert!(eeprom.write(10, 0xAB));
        assert_eq!(eeprom.read(10), Some(0xAB));
    }

    #[test]
    fn test_eeprom_bounds() {
        let eeprom = VirtualEeprom::new(2);
        // Read out of bounds
        assert_eq!(eeprom.read(eeprom.size), None);
        assert_eq!(eeprom.read(99999), None);
    }

    #[test]
    fn test_eeprom_write_bounds() {
        let mut eeprom = VirtualEeprom::new(3);
        // Write out of bounds returns false
        assert!(!eeprom.write(eeprom.size, 0xCC));
        assert!(!eeprom.write(99999, 0xDD));
        // No writes should have been counted
        assert_eq!(eeprom.write_count, 0);
    }

    #[test]
    fn test_eeprom_fill() {
        let mut eeprom = VirtualEeprom::new(4);
        // Write a value first
        assert!(eeprom.write(0, 0x12));
        assert_eq!(eeprom.read(0), Some(0x12));

        // Fill with a pattern
        eeprom.fill(0xFF);
        assert_eq!(eeprom.read(0), Some(0xFF));
        assert_eq!(eeprom.read(100), Some(0xFF));
        assert_eq!(eeprom.read(eeprom.size - 1), Some(0xFF));

        // Fill does not increment write_count
        assert_eq!(eeprom.write_count, 1);
    }

    #[test]
    fn test_eeprom_write_count() {
        let mut eeprom = VirtualEeprom::new(5);
        assert_eq!(eeprom.write_count, 0);
        assert!(eeprom.write(0, 0x01));
        assert_eq!(eeprom.write_count, 1);
        assert!(eeprom.write(0, 0x02));
        assert_eq!(eeprom.write_count, 2);
        assert!(eeprom.write(42, 0x03));
        assert_eq!(eeprom.write_count, 3);
        // Out-of-bounds write does not increment
        assert!(!eeprom.write(eeprom.size, 0x04));
        assert_eq!(eeprom.write_count, 3);
    }

    #[test]
    fn test_eeprom_custom_size() {
        let eeprom = VirtualEeprom::with_size(6, 1024);
        assert_eq!(eeprom.size, 1024);
        assert_eq!(eeprom.data.len(), 1024);
        // Read at max valid address
        assert_eq!(eeprom.read(1023), Some(0));
        // Read one past end
        assert_eq!(eeprom.read(1024), None);
    }

    // ── Flash tests ─────────────────────────────────────────────────────

    #[test]
    fn test_flash_defaults() {
        let flash = VirtualFlash::new(1);
        assert_eq!(flash.page_size, 256);
        assert_eq!(flash.page_count, 64);
        assert_eq!(flash.total_size(), 256 * 64);
        assert_eq!(flash.erase_count.len(), 64);
        assert!(flash.erase_count.iter().all(|&c| c == 0));
        // All bytes are erased (0xFF)
        assert_eq!(flash.read(0), Some(0xFF));
        assert_eq!(flash.read(flash.total_size() - 1), Some(0xFF));
    }

    #[test]
    fn test_flash_page_write() {
        let mut flash = VirtualFlash::new(2);
        let data = &[0xAA, 0xBB, 0xCC];
        // Write to page 0, offset 10
        assert!(flash.write_page(0, 10, data));
        // Verify the written bytes
        assert_eq!(flash.read(10), Some(0xAA));
        assert_eq!(flash.read(11), Some(0xBB));
        assert_eq!(flash.read(12), Some(0xCC));
        // Bytes outside the write region are still erased
        assert_eq!(flash.read(9), Some(0xFF));
        assert_eq!(flash.read(13), Some(0xFF));
    }

    #[test]
    fn test_flash_page_erase() {
        let mut flash = VirtualFlash::new(3);
        // Write some data first
        assert!(flash.write_page(0, 0, &[0x11, 0x22]));
        assert_eq!(flash.read(0), Some(0x11));
        assert_eq!(flash.read(1), Some(0x22));

        // Erase page 0
        assert!(flash.erase_page(0));
        assert_eq!(flash.read(0), Some(0xFF));
        assert_eq!(flash.read(1), Some(0xFF));
        assert_eq!(flash.erase_count[0], 1);
    }

    #[test]
    fn test_flash_write_to_non_erased_fails() {
        let mut flash = VirtualFlash::new(4);
        // Write at offset 5
        assert!(flash.write_page(0, 0, &[0xAA]));
        // Try to write at the same offset again without erasing
        assert!(!flash.write_page(0, 0, &[0xBB]));
        // The original value is preserved
        assert_eq!(flash.read(0), Some(0xAA));
    }

    #[test]
    fn test_flash_bounds() {
        let mut flash = VirtualFlash::new(5);
        // Read out of bounds
        assert_eq!(flash.read(flash.total_size()), None);
        assert_eq!(flash.read(99999), None);

        // Write to out-of-bounds page
        assert!(!flash.write_page(flash.page_count, 0, &[0x00]));

        // Write with offset that exceeds page size
        assert!(!flash.write_page(0, flash.page_size, &[0x00]));

        // Write where offset + data.len() exceeds page_size
        assert!(!flash.write_page(0, flash.page_size - 1, &[0x00, 0x01]));

        // Erase out-of-bounds page
        assert!(!flash.erase_page(flash.page_count));
    }

    #[test]
    fn test_flash_erase_count() {
        let mut flash = VirtualFlash::new(6);
        assert_eq!(flash.erase_count[0], 0);
        assert_eq!(flash.erase_count[1], 0);

        // Write, erase, write again — each erase increments the counter
        assert!(flash.write_page(0, 0, &[0x55]));
        assert!(flash.erase_page(0));
        assert_eq!(flash.erase_count[0], 1);

        assert!(flash.write_page(0, 0, &[0x66]));
        assert!(flash.erase_page(0));
        assert_eq!(flash.erase_count[0], 2);

        // Erase page 1 as well
        assert!(flash.erase_page(1));
        assert_eq!(flash.erase_count[1], 1);

        // Out-of-bounds erase does not increment
        assert!(!flash.erase_page(flash.page_count));
        assert_eq!(flash.erase_count[0], 2);
    }

    #[test]
    fn test_flash_write_empty_data() {
        let mut flash = VirtualFlash::new(7);
        // Writing zero-length data should always succeed
        assert!(flash.write_page(0, 0, &[]));
        assert!(flash.write_page(0, 255, &[]));
    }

    #[test]
    fn test_flash_custom_geometry() {
        let flash = VirtualFlash::with_geometry(8, 512, 8);
        assert_eq!(flash.page_size, 512);
        assert_eq!(flash.page_count, 8);
        assert_eq!(flash.total_size(), 4096);
        assert_eq!(flash.erase_count.len(), 8);
        assert_eq!(flash.read(0), Some(0xFF));
        assert_eq!(flash.read(4095), Some(0xFF));
    }
}
