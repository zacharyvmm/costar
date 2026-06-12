# Universal RTOS Native Simulator

A deterministic, single-threaded, cross-platform simulator that executes FreeRTOS C firmware on Rust-managed stackful fibers, with virtual-time event scheduling.

**Status:** MVP — FreeRTOS tasks with queues, delays, virtual devices, deterministic networking.

## Quick Start

```bash
# Build
cargo build

# Run all tests
cargo test --workspace

# Run the demo (two-task FreeRTOS queue ping-pong)
cargo run

# Golden trace output (machine-readable, for CI comparison)
cargo run -- --golden

# Wall-clock watchdog (warn if simulation exceeds N seconds)
cargo run -- --watchdog 5

# Format + lint
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

## Architecture

| Layer | Crate | Responsibility |
|-------|-------|---------------|
| Simulation Core | `crates/sim-core/` | Virtual time (`Tick`), deterministic min-heap event queue, trace sink, run loop |
| Fiber Runtime | `crates/sim-fiber/` | Stackful coroutines via `corosensei`, TLS active yielder, task state machine |
| C ABI Bridge | `crates/sim-ffi/` | `#[no_mangle]` exports called by C, thread-local global state |
| Virtual Devices | `crates/sim-devices/` | IRQ controller, virtual timer/UART/GPIO, `inventory`-based driver registry |
| Networking | `crates/sim-net/` | Deterministic smoltcp device (rx/tx queues), packet inject/drain/trace |
| FreeRTOS Port | `crates/sim-freertos-port/` | `port.c`, `portmacro.h`, `build.rs` (compiles C payload via `cc` crate) |
| Runner | `crates/sim-runner/` | Host binary linking C firmware + Rust engine |
| Guest C | `c_firmware/` | FreeRTOS kernel (`task.c`, `queue.c`, `list.c`) and application (`app/main.c`) |

### Key Design Decisions

- **Rust owns fiber lifecycle and scheduling.** C FreeRTOS maintains TCB metadata (ready/delay lists) as auxiliary state kept in sync via bridge functions.
- **One host thread, no async/await.** The simulator runs on a single host thread. All RTOS tasks map to stackful fibers — the C payload expects blocking call stacks.
- **Virtual time, not wall time.** All timers, sleeps, and events are scheduled against a monotonic `u64` tick counter. Wall-clock time is only used for the optional watchdog.

## Features (MVP)

- [x] Deterministic min-heap event queue (timestamp → priority → sequence)
- [x] Stackful fibers via `corosensei` with TLS active yielder for C hooks
- [x] FreeRTOS kernel subset: tasks, queues, delays, `vTaskDelayUntil`, critical sections
- [x] Virtual tick interrupt with delayed-task wakeup
- [x] Virtual devices: UART (with trace), timer (one-shot/periodic with IRQ), GPIO (IRQ-on-change)
- [x] `inventory`-based compile-time driver registration (sorted init)
- [x] Deterministic networking: smoltcp device with rx/tx queues, packet trace
- [x] Golden trace capture and comparison tests
- [x] CLI: `--golden`, `--watchdog`, `--help`

## Limitations

Per HANDOFF.md §19, the MVP has the following known limitations:

1. **No arbitrary loop preemption.** Stackful fibers are cooperative. Tight infinite loops (`while(1){}`) will freeze the simulator. C code must eventually call an RTOS blocking primitive (yield, delay, queue receive, etc.).
2. **C undefined behavior is not sandboxed.** The simulator runs firmware in the same process. A wild pointer in C can corrupt the Rust engine. Run sanitizer builds in CI where available.
3. **Host-connected networking is not deterministic.** The deterministic networking mode uses in-memory packet injection via `sim_net_inject_rx`. Host sockets via `polling` are a future feature and will be non-deterministic.
4. **Zephyr support is future work.** The MVP targets FreeRTOS only. Zephyr requires a separate feasibility phase.
5. **No process isolation for untrusted firmware.** All simulated tasks share one host process.
6. **Panic boundary is incomplete.** Rust panics currently propagate through coroutines; `catch_unwind` is planned for production.

## Supported Platforms

- Linux x86_64 (primary)
- macOS x86_64 (planned)
- macOS Apple Silicon (planned)
- Windows MSVC x86/x86_64 (planned)

CI covers Linux; other platforms need runner setup (see `.github/workflows/ci.yml`).

## Running Tests

```bash
# Full test suite (60 tests)
cargo test --workspace

# Golden trace test (compares output to expected trace)
bash tests/golden_trace_test.sh

# Specific crate
cargo test -p sim-core
cargo test -p sim-net
cargo test -p sim-ffi
```

## Project Structure

```
crates/
  sim-core/        Simulation core (time, event queue, trace, run loop)
  sim-fiber/       Fiber runtime (coroutines, TLS yielder, task states)
  sim-ffi/         C ABI bridge (no_mangle exports, global state, scheduler)
  sim-devices/     Virtual devices (IRQ, timer, UART, GPIO, registry)
  sim-net/         Networking (smoltcp device, host poller stub)
  sim-freertos-port/ FreeRTOS port layer (port.c, portmacro.h, build.rs)
  sim-runner/      Host binary (main.rs, CLI)

c_firmware/
  app/main.c       Demo application (two-task queue ping-pong)
  freertos/        Minimal FreeRTOS kernel (task.c, queue.c, list.c)

docs/
  HANDOFF.md       Full design document and implementation plan
  IMPLEMENTATION_STATUS.md  Per-phase checklist

tests/
  golden_trace_test.sh  Golden trace comparator
  traces/                Expected trace files
```

## License

MIT OR Apache-2.0
