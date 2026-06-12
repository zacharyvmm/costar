# Universal RTOS Native Simulator

A deterministic, single-threaded, cross-platform simulator that executes FreeRTOS C firmware on Rust-managed stackful fibers, with virtual-time event scheduling.

**Status:** MVP — FreeRTOS tasks with queues, delays, virtual devices, deterministic networking, and Tier 3 edge instrumentation.

## Quick Start

```bash
# Build
cargo build

# Run all tests (83 tests)
cargo test --workspace

# Run the demo (two-task FreeRTOS queue ping-pong)
cargo run

# Golden trace output (machine-readable, for CI comparison)
cargo run -- --golden

# Wall-clock watchdog (warn if simulation exceeds N seconds)
cargo run -- --watchdog 5

# Run the interactive demo (host I/O with socketpair)
cargo run -- --mode interactive

# Run the Tier 3 tight-loop demo (edge instrumentation — requires Clang)
SIM_INSTRUMENT_EDGES=1 cargo run -- --mode tight-loop

# Run with TOML config file
cargo run -- --config sim.toml

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
| Guest C | `c_firmware/` | FreeRTOS kernel (`task.c`, `queue.c`, `list.c`, `timers.c`) and application (`app/main.c`) |

### Key Design Decisions

- **Rust owns fiber lifecycle and scheduling.** C FreeRTOS maintains TCB metadata (ready/delay lists) as auxiliary state kept in sync via bridge functions.
- **One host thread, no async/await.** The simulator runs on a single host thread. All RTOS tasks map to stackful fibers — the C payload expects blocking call stacks.
- **Virtual time, not wall time.** All timers, sleeps, and events are scheduled against a monotonic `u64` tick counter. Wall-clock time is only used for the optional watchdog.

## Cooperative Scheduling with Instrumented Yield Points

The simulator uses a cooperative scheduling model at the fiber/runtime level: simulated RTOS tasks run until they block, sleep, yield, perform certain RTOS operations, or reach an instrumentation checkpoint. This keeps execution deterministic and avoids relying on host threads, host timers, or platform-specific preemption behavior.

For normal RTOS firmware, execution is handed back to the Rust scheduler through the RTOS port layer. Calls such as task delay, queue receive/send, explicit yield, timer waits, and other blocking primitives suspend the active fiber and allow the simulator to advance virtual time or run another task.

For CPU-bound C code, the simulator also supports optional instrumentation-assisted scheduling:

**Tier 1 — Function-entry instrumentation** (`SIM_INSTRUMENT_FUNCTIONS=1`): Compatible C compilers (GCC/Clang) insert `__cyg_profile_func_enter` hooks at every function entry. These call `sim_budget_poll`, which increments a counter and yields the fiber with `BudgetExceeded` if the budget limit is reached. This makes long-running firmware code much safer to execute without requiring every scheduling point to be a direct RTOS API call.

**Tier 2 — Manual loop hooks** (`SIM_LOOP_POLL()` macro): For tight loops that do not naturally call functions or RTOS primitives, firmware can use `SIM_LOOP_POLL()` from `sim_abi.h`. This provides an explicit low-cost checkpoint inside loops such as polling loops, protocol parsers, busy-wait compatibility shims, or test workloads.

**Tier 3 — Edge instrumentation** (`SIM_INSTRUMENT_EDGES=1`): With Clang's `-fsanitize-coverage=trace-pc-guard`, the compiler inserts `__sanitizer_cov_trace_pc_guard` callbacks at every basic-block edge. After a fast thread-local throttle (default: every 10,000 edges), these call `sim_budget_poll`. This is the only tier that can preempt a tight `while(1){}` loop with zero function calls and zero manual checkpoints — enabling robust infinite-loop control for cooperative fibers.

This means the simulator is cooperative by design, but not limited to only hand-written yield calls. In instrumented builds, control can be returned to the scheduler automatically at function-entry boundaries (Tier 1), manually via loop checkpoints (Tier 2), or at edge-level granularity via compiler instrumentation (Tier 3).

## Features (MVP)

- [x] Deterministic min-heap event queue (timestamp → priority → sequence)
- [x] Stackful fibers via `corosensei` with TLS active yielder for C hooks
- [x] FreeRTOS kernel: tasks, queues, delays, `vTaskDelayUntil`, critical sections, software timers
- [x] Virtual tick interrupt with delayed-task wakeup + tickless idle fast-forward
- [x] Virtual devices: UART (trace-backed), timer (one-shot/periodic with IRQ), GPIO (IRQ-on-change), IRQ controller
- [x] `inventory`-based compile-time driver registration (sorted init)
- [x] Deterministic networking: smoltcp device with rx/tx queues, packet trace
- [x] Host-connected I/O: `polling`-based non-blocking sockets, interactive mode (`--mode interactive`)
- [x] Native Rust task API: `spawn_rust_task` with `TaskContext` (yield, sleep, now)
- [x] Public `Simulator` API (§14): `run`, `run_until`, `run_until_idle`, `schedule_at`, `cancel`
- [x] Golden trace capture and comparison tests
- [x] Panic boundary: `catch_unwind` catches Rust panics in fibers, marks task Faulted
- [x] Function-entry instrumentation (Tier 1): `sim_budget_poll`, `SIM_INSTRUMENT_FUNCTIONS` build flag
- [x] Manual loop hooks (Tier 2): `SIM_LOOP_POLL()` macro in `sim_abi.h`
- [x] Edge instrumentation (Tier 3): `-fsanitize-coverage=trace-pc-guard` via Clang, `SIM_INSTRUMENT_EDGES` build flag, tight-loop demo (`--mode tight-loop`)
- [x] CLI: `--golden`, `--watchdog`, `--mode`, `--rtos`, `--config`, `--help`
- [x] TOML config file support with serde deserialization
- [x] Zephyr hello-thread PoC (standalone test, no Zephyr SDK)

## Limitations

Per HANDOFF.md §19, the MVP has the following known limitations:

1. **Cooperative at fiber level — mitigated by instrumentation.** The simulator does not provide arbitrary instruction-level preemption. RTOS blocking calls, explicit yields, delays, queue operations, function-entry instrumentation (`SIM_INSTRUMENT_FUNCTIONS=1`), manual `SIM_LOOP_POLL()` checkpoints, and edge instrumentation (`SIM_INSTRUMENT_EDGES=1`) can all return control to the scheduler. Without any instrumentation, a tight infinite loop with no function calls, no RTOS calls, and no manual checkpoint can still freeze the simulator, though Tier 3 edge instrumentation covers the most important case (tight `while(1){}` loops).
2. **C undefined behavior is not sandboxed.** The simulator runs firmware in the same process. A wild pointer in C can corrupt the Rust engine. Run sanitizer builds in CI where available.
3. **Host-connected networking is not deterministic.** Host sockets via `polling` are available in interactive mode but are not guaranteed bit-for-bit deterministic. Deterministic networking uses in-memory packet injection via `sim_net_inject_rx`.
4. **Zephyr support is early-stage.** The MVP targets FreeRTOS primarily. A Zephyr hello-thread PoC and feasibility design document are complete. Full Zephyr SDK integration is future work.
5. **No process isolation for untrusted firmware.** All simulated tasks share one host process.

## Supported Platforms

- Linux x86_64 (verified — CI)
- macOS x86_64 (verified)
- macOS Apple Silicon (verified — macOS 26.5.1)
- Windows MSVC x86/x86_64 (planned)

CI covers Linux; macOS verified manually; Windows needs runner setup (see `.github/workflows/ci.yml`).

## Running Tests

```bash
# Full test suite (83 tests)
cargo test --workspace

# Golden trace test (compares output to expected traces)
bash tests/golden_trace_test.sh all

# Edge-instrumented tight-loop demo (requires Clang)
SIM_INSTRUMENT_EDGES=1 cargo run -- --mode tight-loop

# Specific crate
cargo test -p sim-core
cargo test -p sim-net
cargo test -p sim-ffi
```

## Project Structure

```
crates/
  sim-core/          Simulation core (time, event queue, trace, run loop)
  sim-fiber/         Fiber runtime (coroutines, TLS yielder, task states)
  sim-ffi/           C ABI bridge (no_mangle exports, global state, scheduler)
  sim-devices/       Virtual devices (IRQ, timer, UART, GPIO, registry)
  sim-net/           Networking (smoltcp device, host poller)
  sim-freertos-port/ FreeRTOS port layer (port.c, portmacro.h, build.rs,
                     sim_coverage.c, sim_hooks.c, sim_kernel_bridge.c)
  sim-zephyr-port/   Zephyr port layer (zephyr_arch.c, thread registry,
                     zephyr_integration/ board definition files)
  sim-runner/        Host binary (main.rs, CLI)

c_firmware/
  app/
    main.c                Deterministic demo (queue ping-pong)
    main_interactive.c    Interactive demo (socketpair host I/O)
    tight_loop_demo.c     Tier 3 edge-instrumentation demo
  freertos/               Real FreeRTOS kernel (task.c, queue.c, list.c, timers.c)
  zephyr_app/
    standalone_test.c     Zephyr hello-thread demo (no Zephyr SDK)

docs/
  HANDOFF.md              Full design document and implementation plan
  IMPLEMENTATION_STATUS.md Per-phase checklist

tests/
  golden_trace_test.sh    Golden trace comparator
  traces/
    expected_queue_ping_pong.trace  FreeRTOS golden (40 events)
    expected_zephyr_hello.trace     Zephyr golden (12 events)
    expected_tight_loop.trace       Tier 3 edge-instrumented golden (335 events)
```

## License

MIT OR Apache-2.0
