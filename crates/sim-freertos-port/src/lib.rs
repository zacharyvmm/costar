//! # sim-freertos-port
//!
//! Rust side of the custom FreeRTOS simulator port.
//!
//! The C side (port.c, portmacro.h, sim_hooks.c) is compiled via the `cc`
//! crate in `build.rs`.  This Rust side wires the FreeRTOS port hooks to
//! the sim-ffi ABI and the fiber runtime.

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
