//! Virtual entropy source.
//!
//! This module provides a deterministic pseudo-random number generator
//! that fills byte buffers with repeatable output for simulation testing.
//!
//! The [`VirtualEntropy`] device uses a 64-bit xorshift+ generator seeded
//! by the simulator so that the same seed always produces the same byte
//! sequence — essential for golden-trace determinism.

/// A virtual entropy source backed by a seeded xorshift128+ PRNG.
///
/// The generator produces 64-bit values that are decomposed into bytes
/// via little-endian encoding.  This is NOT a cryptographic entropy
/// source; it exists so C firmware that reads entropy (e.g. for
/// nonce/random-number generation) produces deterministic traces.
///
/// # Determinism guarantee
///
/// This device is deliberately NOT seeded from host entropy.
/// For a given seed value, `request_bytes` must always produce the
/// same byte sequence regardless of host OS, wall-clock time, or
/// process scheduling.
#[derive(Debug, Clone)]
pub struct VirtualEntropy {
    /// Entropy device ID.
    pub id: u32,
    /// Xorshift128+ state — 2 × u64.
    state: [u64; 2],
    /// Total bytes requested (for trace/inspection).
    pub bytes_generated: u64,
}

impl VirtualEntropy {
    /// Create a new entropy source with the given ID.
    ///
    /// The initial seed is fixed (0xCAFEF00D_D15EA5ED, 0x01234567_89ABCDEF).
    /// Call [`seed`](Self::seed) to reseed with a custom value.
    pub fn new(id: u32) -> Self {
        Self {
            id,
            state: [0xCAFEF00D_D15EA5ED, 0x01234567_89ABCDEF],
            bytes_generated: 0,
        }
    }

    /// Reseed the generator.
    ///
    /// The provided `seed` is mixed into `state[1]`; both state words are
    /// then stirred so that the output sequence changes from the first call.
    pub fn seed(&mut self, seed: u64) {
        self.state[0] = seed ^ 0x9E3779B97F4A7C15;
        self.state[1] = seed.wrapping_add(0x9E3779B97F4A7C15);
        // Discard the first value so successive `new(…) + seed(N)`
        // calls diverge.
        self.next_u64();
    }

    /// Fill `buf` with deterministic pseudo-random bytes.
    ///
    /// Returns the number of bytes actually written (always `buf.len()`).
    pub fn request_bytes(&mut self, buf: &mut [u8]) -> usize {
        let mut remaining = buf.len();
        let mut offset = 0;

        while remaining > 0 {
            let r = self.next_u64();
            let bytes = r.to_le_bytes();
            let n = remaining.min(8);
            buf[offset..offset + n].copy_from_slice(&bytes[..n]);
            offset += n;
            remaining -= n;
        }

        self.bytes_generated += buf.len() as u64;
        buf.len()
    }

    /// Return the next 64-bit pseudo-random value (xorshift128+).
    fn next_u64(&mut self) -> u64 {
        let mut s1 = self.state[0];
        let s0 = self.state[1];
        self.state[0] = s0;

        s1 ^= s1 << 23;
        s1 ^= s1 >> 18;
        s1 ^= s0 ^ (s0 >> 5);
        self.state[1] = s1;

        s1.wrapping_add(s0)
    }

    /// Reset the device to its initial state (default seed, zero counters).
    pub fn reset(&mut self) {
        self.state = [0xCAFEF00D_D15EA5ED, 0x01234567_89ABCDEF];
        self.bytes_generated = 0;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_creation_defaults() {
        let ent = VirtualEntropy::new(0);
        assert_eq!(ent.id, 0);
        assert_eq!(ent.bytes_generated, 0);
    }

    #[test]
    fn test_request_bytes_deterministic() {
        let mut e1 = VirtualEntropy::new(0);
        let mut e2 = VirtualEntropy::new(0);

        let mut b1 = [0u8; 16];
        let mut b2 = [0u8; 16];
        e1.request_bytes(&mut b1);
        e2.request_bytes(&mut b2);

        assert_eq!(b1, b2, "same device → same output");
        assert_eq!(e1.bytes_generated, 16);
    }

    #[test]
    fn test_seed_changes_output() {
        let mut e1 = VirtualEntropy::new(0);
        let mut e2 = VirtualEntropy::new(0);
        e2.seed(42);

        let mut b1 = [0u8; 16];
        let mut b2 = [0u8; 16];
        e1.request_bytes(&mut b1);
        e2.request_bytes(&mut b2);

        assert_ne!(b1, b2, "different seeds → different output");
    }

    #[test]
    fn test_same_seed_same_output() {
        let mut e1 = VirtualEntropy::new(0);
        e1.seed(12345);
        let mut e2 = VirtualEntropy::new(1);
        e2.seed(12345);

        let mut b1 = [0u8; 32];
        let mut b2 = [0u8; 32];
        e1.request_bytes(&mut b1);
        e2.request_bytes(&mut b2);

        assert_eq!(b1, b2);
    }

    #[test]
    fn test_request_bytes_small_buffer() {
        let mut e = VirtualEntropy::new(0);
        let mut buf = [0u8; 1];
        let n = e.request_bytes(&mut buf);
        assert_eq!(n, 1);
        assert_eq!(e.bytes_generated, 1);
    }

    #[test]
    fn test_request_bytes_large_buffer() {
        let mut e = VirtualEntropy::new(0);
        let mut buf = vec![0u8; 1024];
        let n = e.request_bytes(&mut buf);
        assert_eq!(n, 1024);
        assert_eq!(e.bytes_generated, 1024);

        // Not all zeros — basic sanity check that PRNG actually runs.
        let has_nonzero = buf.iter().any(|&b| b != 0);
        assert!(has_nonzero, "random output should contain non-zero bytes");
    }

    #[test]
    fn test_reset_restores_default() {
        let mut e = VirtualEntropy::new(0);
        e.seed(999);
        let mut after_seed = [0u8; 8];
        e.request_bytes(&mut after_seed);

        e.reset();
        let mut after_reset = [0u8; 8];
        e.request_bytes(&mut after_reset);

        // After reset, first 8 bytes should match the default seed's output.
        let mut expected = [0u8; 8];
        VirtualEntropy::new(0).request_bytes(&mut expected);
        assert_eq!(after_reset, expected);
        assert_ne!(after_seed, after_reset);
    }

    #[test]
    fn test_consecutive_calls_differ() {
        let mut e = VirtualEntropy::new(0);
        let mut b1 = [0u8; 16];
        let mut b2 = [0u8; 16];
        e.request_bytes(&mut b1);
        e.request_bytes(&mut b2);
        assert_ne!(b1, b2, "consecutive calls should produce different output");
    }

    #[test]
    fn test_zero_length_request() {
        let mut e = VirtualEntropy::new(0);
        let n = e.request_bytes(&mut []);
        assert_eq!(n, 0);
        assert_eq!(e.bytes_generated, 0);
    }
}
