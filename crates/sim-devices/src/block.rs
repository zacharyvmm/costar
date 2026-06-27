//! Virtual block device for filesystem backends (littlefs, FAT).

use std::io;

/// Default page size in bytes.
pub const BLOCK_DEFAULT_PAGE_SIZE: u32 = 512;

/// Default page count.
pub const BLOCK_DEFAULT_PAGE_COUNT: u32 = 64;

/// A deterministic, page-addressed virtual block device.
pub struct FlatMemoryStore {
    pub id: u32,
    pub page_size: u32,
    pub page_count: u32,
    pages: Vec<u8>,
    pub write_counts: Vec<u64>,
    pub erase_counts: Vec<u64>,
    pub erase_value: u8,
}

impl FlatMemoryStore {
    pub fn new(id: u32, page_size: u32, page_count: u32, erase_value: u8) -> Self {
        let total = (page_size * page_count) as usize;
        Self {
            id,
            page_size,
            page_count,
            pages: vec![erase_value; total],
            write_counts: vec![0; page_count as usize],
            erase_counts: vec![0; page_count as usize],
            erase_value,
        }
    }

    pub fn total_size(&self) -> u32 {
        self.page_size * self.page_count
    }

    /// Read bytes from absolute offset. Returns bytes actually read (clamped to bounds).
    pub fn read(&self, offset: u32, buf: &mut [u8]) -> u32 {
        let total = self.total_size();
        if offset >= total {
            return 0;
        }
        let available = (total - offset) as usize;
        let len = buf.len().min(available);
        let start = offset as usize;
        buf[..len].copy_from_slice(&self.pages[start..start + len]);
        len as u32
    }

    /// Write bytes at absolute offset. Before writing, target locations must be erased.
    /// Returns bytes actually written. Increments write_counts for affected pages.
    pub fn write(&mut self, offset: u32, data: &[u8]) -> u32 {
        let total = self.total_size();
        if offset >= total || data.is_empty() {
            return 0;
        }
        let available = (total - offset) as usize;
        let len = data.len().min(available);
        let start = offset as usize;
        // Check all target bytes are erased
        for i in 0..len {
            if self.pages[start + i] != self.erase_value {
                return 0;
            }
        }
        self.pages[start..start + len].copy_from_slice(&data[..len]);
        // Increment write_counts for each affected page
        let first_page = (offset / self.page_size) as usize;
        let last_page = ((offset + len as u32 - 1) / self.page_size) as usize;
        for p in first_page..=last_page.min(self.page_count as usize - 1) {
            self.write_counts[p] += 1;
        }
        len as u32
    }

    /// Erase the page containing `offset`. Sets all bytes in that page to erase_value.
    pub fn erase_page(&mut self, offset: u32) -> bool {
        if offset >= self.total_size() {
            return false;
        }
        let page = (offset / self.page_size) as usize;
        let start = page * self.page_size as usize;
        let end = start + self.page_size as usize;
        self.pages[start..end].fill(self.erase_value);
        self.erase_counts[page] += 1;
        true
    }

    /// Fill the entire device with erase_value (factory reset). Does NOT increment counters.
    pub fn fill(&mut self) {
        self.pages.fill(self.erase_value);
    }

    /// Save state to a host file path. Returns Ok(()) on success.
    pub fn snapshot(&self, path: &str) -> io::Result<()> {
        std::fs::write(path, &self.pages)
    }

    /// Restore state from a host file path.
    pub fn restore(
        id: u32,
        page_size: u32,
        page_count: u32,
        erase_value: u8,
        path: &str,
    ) -> io::Result<Self> {
        let data = std::fs::read(path)?;
        let expected = (page_size * page_count) as usize;
        let mut pages = vec![erase_value; expected];
        let copy_len = data.len().min(expected);
        pages[..copy_len].copy_from_slice(&data[..copy_len]);
        let write_counts = vec![0; page_count as usize];
        let erase_counts = vec![0; page_count as usize];
        Ok(Self {
            id,
            page_size,
            page_count,
            pages,
            write_counts,
            erase_counts,
            erase_value,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_create() {
        let b = FlatMemoryStore::new(0, 512, 64, 0xFF);
        assert_eq!(b.page_size, 512);
        assert_eq!(b.page_count, 64);
        assert_eq!(b.total_size(), 32768);
        assert_eq!(b.erase_value, 0xFF);
    }

    #[test]
    fn test_block_read_write() {
        let mut b = FlatMemoryStore::new(1, 256, 4, 0xFF);
        let data = [0xAA, 0xBB, 0xCC];
        assert_eq!(b.write(0, &data), 3);
        let mut buf = [0u8; 3];
        assert_eq!(b.read(0, &mut buf), 3);
        assert_eq!(buf, data);
    }

    #[test]
    fn test_block_write_non_erased_fails() {
        let mut b = FlatMemoryStore::new(2, 256, 4, 0xFF);
        assert_eq!(b.write(0, &[0x11]), 1);
        assert_eq!(b.write(0, &[0x22]), 0); // already written
    }

    #[test]
    fn test_block_erase_and_rewrite() {
        let mut b = FlatMemoryStore::new(3, 256, 4, 0xFF);
        assert_eq!(b.write(100, &[0x55]), 1);
        assert!(b.erase_page(100));
        let mut buf = [0u8; 1];
        assert_eq!(b.read(100, &mut buf), 1);
        assert_eq!(buf[0], 0xFF);
        assert_eq!(b.erase_counts[0], 1);
        // Now can write again
        assert_eq!(b.write(100, &[0x77]), 1);
        assert_eq!(b.read(100, &mut buf), 1);
        assert_eq!(buf[0], 0x77);
    }

    #[test]
    fn test_block_bounds() {
        let mut b = FlatMemoryStore::new(4, 256, 4, 0xFF);
        // read past end
        let mut buf = [0u8; 10];
        assert_eq!(b.read(b.total_size(), &mut buf), 0);
        // write past end
        assert_eq!(b.write(b.total_size(), &[0xAA]), 0);
        // erase past end
        assert!(!b.erase_page(b.total_size()));
    }

    #[test]
    fn test_block_write_across_page_boundary() {
        let mut b = FlatMemoryStore::new(5, 256, 4, 0xFF);
        // write 3 bytes at page boundary
        let data = [0x01, 0x02, 0x03];
        assert_eq!(b.write(255, &data), 3);
        let mut buf = [0u8; 3];
        assert_eq!(b.read(255, &mut buf), 3);
        assert_eq!(buf, data);
        // write_counts for both pages incremented
        assert_eq!(b.write_counts[0], 1);
        assert_eq!(b.write_counts[1], 1);
    }

    #[test]
    fn test_block_snapshot_restore() {
        let mut b = FlatMemoryStore::new(6, 256, 4, 0xFF);
        b.write(0, &[0xDE, 0xAD]);
        // snapshot to temp file
        let path = std::env::temp_dir().join("costar_block_test_snapshot.bin");
        b.snapshot(path.to_str().unwrap()).unwrap();
        // restore
        let b2 = FlatMemoryStore::restore(6, 256, 4, 0xFF, path.to_str().unwrap()).unwrap();
        let mut buf = [0u8; 2];
        b2.read(0, &mut buf);
        assert_eq!(buf, [0xDE, 0xAD]);
        // cleanup
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_block_fill() {
        let mut b = FlatMemoryStore::new(7, 256, 4, 0xFF);
        b.write(0, &[0xAA; 10]);
        b.fill();
        let mut buf = [0u8; 1];
        b.read(0, &mut buf);
        assert_eq!(buf[0], 0xFF);
    }

    #[test]
    fn test_block_erase_counts() {
        let mut b = FlatMemoryStore::new(8, 256, 4, 0xFF);
        assert!(b.erase_page(0));
        b.write(0, &[0x01]);
        b.erase_page(0);
        b.write(0, &[0x02]);
        b.erase_page(0);
        assert_eq!(b.erase_counts[0], 3);
        // Page 1 untouched
        assert_eq!(b.erase_counts[1], 0);
    }
}
