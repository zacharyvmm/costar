# Contributing to costar

Thank you for your interest in contributing! This guide covers the essentials
for building, testing, and submitting changes.

## Prerequisites

| Requirement | Details |
|-------------|---------|
| **Rust** | MSRV **1.84** (stable toolchain recommended) |
| **C compiler** | GCC or Clang — required for FreeRTOS/Zephyr C firmware |
| **Platform** | Linux x86_64, macOS (x86_64 / Apple Silicon), Windows MSVC |

Optional:

- **Clang** — needed for Tier 3 edge instrumentation (`SIM_INSTRUMENT_EDGES=1`)
- **Zephyr SDK** — only required for real Zephyr kernel builds (`zephyr_real` feature)

## Building

```bash
cargo build              # Debug build (all crates)
cargo build --release    # Optimised build
```

The workspace includes C code compiled via the `cc` crate; no separate CMake or
Makefile step is needed.

## Running Tests

```bash
# Full test suite (~320 tests)
cargo test --workspace

# Golden trace tests (compares output to expected traces)
bash tests/golden_trace_test.sh all

# Scenario golden trace tests (multi-machine simulations)
bash tests/scenario_golden_test.sh

# Single crate
cargo test -p sim-core
```

## Project Architecture

The project is organised as a Cargo workspace under `crates/`. Each crate has
a focused responsibility — see the **Architecture** table in the
[README](README.md) for the full breakdown.

Key crates:

- **sim-core** — virtual time, event queue, trace sink, run loop
- **sim-fiber** — stackful coroutines (corosensei), TLS yielder
- **sim-ffi** — C ABI bridge (`#[no_mangle]` exports)
- **sim-devices** — virtual peripherals (UART, GPIO, I2C, SPI, CAN, …)
- **sim-net** — networking (smoltcp, TCP/TAP bridges)
- **sim-world** — multi-machine orchestration, scenario DSL
- **sim-runner** — CLI, JSON-RPC server, shell, replay
- **sim-grpc** — gRPC server for Electron GUI frontend

## Code Style

### Formatting

All Rust code must pass `cargo fmt --check`. Run `cargo fmt` before committing.

### Linting

Clippy is enforced in CI with `-D warnings`:

```bash
cargo clippy --all-targets -- -D warnings
```

Fix all warnings before submitting a pull request.

### Documentation

Several crates enforce `#![warn(missing_docs)]`. When adding public items,
include a doc comment (`///` or `//!`). Check locally with:

```bash
cargo doc --workspace --no-deps
```

## Submitting Changes

1. Fork the repository and create a feature branch.
2. Make your changes — keep commits focused and well-described.
3. Ensure `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
   `cargo test --workspace` all pass.
4. Open a pull request against `main` with a clear description of what changed
   and why.

## License

By contributing, you agree that your contributions will be licensed under the
same terms as the project: **MIT OR Apache-2.0**.
