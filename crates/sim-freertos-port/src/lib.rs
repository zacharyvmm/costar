//! # sim-freertos-port
//!
//! Compiles the FreeRTOS kernel C source code and the custom simulator port
//! layer via `build.rs` using the [`cc`] crate.
//!
//! This crate does **not** export any Rust items.  All value is in the build
//! script, which:
//!
//! - Compiles the FreeRTOS kernel (`tasks.c`, `queue.c`, `list.c`, `timers.c`,
//!   `event_groups.c`) with a dynamically-patched `tasks.c` that adds the
//!   simulator bridge hooks (`sim_port_task_created`,
//!   `sim_bridge_create_pending_fibers`, `sim_task_delay_until`).
//! - Compiles the port layer (`port.c`, `sim_hooks.c`, `sim_kernel_bridge.c`,
//!   `sim_block.c`, `sim_eth.c`) that wires FreeRTOS to the `sim-ffi` ABI.
//! - Compiles guest firmware application files from `c_firmware/app/`.
//! - Optionally compiles the FreeRTOS+TCP stack when `SIM_TCP=1`.
//! - Supports opt-in Clang-based coverage instrumentation (`SIM_INSTRUMENT_EDGES=1`)
//!   and function-entry instrumentation (`SIM_INSTRUMENT_FUNCTIONS=1`).
//!
//! The resulting static library (`libembedded_c_payload.a`) is linked into
//! the final `sim-runner` binary.
