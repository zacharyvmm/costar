//! # sim-fiber
//!
//! Stackful coroutine runtime that hosts simulated RTOS tasks.
//!
//! Uses `corosensei` for stack switching and exposes a safe-ish Rust
//! wrapper around task creation, resume, suspend, exit, and fault handling.
//!
//! This crate owns:
//! * Fiber creation and lifecycle
//! * Thread-local active yielder
//! * Panic boundary between Rust and C guest code
//! * Task budget tracking for stall mitigation

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
