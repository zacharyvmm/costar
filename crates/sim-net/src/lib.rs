//! # sim-net
//!
//! Networking layer: deterministic smoltcp integration and optional host poller.
//!
//! Two modes:
//! 1. **Deterministic**: in-process smoltcp with scripted packet I/O
//! 2. **Host-connected**: non-blocking sockets via `polling` or `mio`

pub fn add(left: u64, right: u64) -> u64 {
    left + right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}
