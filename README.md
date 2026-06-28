# costar — Cooperative Scheduler Testing And Runtime

[![CI](https://github.com/zacharyvmm/costar/actions/workflows/ci.yml/badge.svg)](https://github.com/zacharyvmm/costar/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/zacharyvmm/costar/graph/badge.svg)](https://codecov.io/gh/zacharyvmm/costar)

A deterministic, single-threaded, cross-platform simulator that executes FreeRTOS and Zephyr C firmware on Rust-managed stackful fibers, with virtual-time event scheduling and multi-machine World orchestration.

**Status:** post-MVP — 320 tests (315 unit + 5 integration/doc), 15 golden trace targets (12 always-pass, 3 conditional), 6 scenario tests, JSON-RPC server, multi-machine simulation with plant models and CAN bus topology. FreeRTOS+TCP, smoltcp bridge, TAP bridge, BLE scenario DSL, and filesystem block device complete.

## Quick Start

```bash
# Build
cargo build

# Run all tests (320 tests)
cargo test --workspace

# Run the demo (two-task FreeRTOS queue ping-pong)
cargo run

# Golden trace output (machine-readable, for CI comparison)
cargo run -- --golden

# Wall-clock watchdog (warn if simulation exceeds N seconds)
cargo run -- --watchdog 5

# Run the interactive demo (host I/O with TCP loopback)
cargo run -- --mode interactive

# Run the Tier 3 tight-loop demo (edge instrumentation — requires Clang)
SIM_INSTRUMENT_EDGES=1 cargo run -- --mode tight-loop

# Run with TOML config file
cargo run -- --config sim.toml

# Run a multi-machine scenario
cargo run -- --scenario tests/scenarios/ping_pong.toml

# Headless CI test runner
cargo run -- test --all

# JSON-RPC server (for programmatic control)
cargo run -- serve --stdio

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
| Virtual Devices | `crates/sim-devices/` | IRQ controller, virtual timer/UART/GPIO/I2C/SPI/CAN, ADC, TempSensor, EEPROM, Flash, FaultInjector, Entropy, `inventory`-based driver registry |
| Networking | `crates/sim-net/` | Deterministic smoltcp device (rx/tx queues), packet inject/drain/trace |
| FreeRTOS Port | `crates/sim-freertos-port/` | `port.c`, `portmacro.h`, `build.rs` (compiles C payload via `cc` crate) |
| Zephyr Port | `crates/sim-zephyr-port/` | Zephyr arch layer, cc crate kernel compilation, west build support, ztest integration |
| Runner | `crates/sim-runner/` | Host binary linking C firmware + Rust engine, CLI, JSON-RPC server |
| gRPC Server | `crates/sim-grpc/` | gRPC server (tonic + protobuf) exposing session lifecycle, scenario loading, board configuration, device inspection, keyframes, display streaming, and a bidirectional Run stream — remote API for the Electron GUI frontend |
| World | `crates/sim-world/` | Multi-machine orchestration, CAN bus topology, links, plant models, firmware trait, scenario DSL |
| Guest C | `c_firmware/` | FreeRTOS kernel (`task.c`, `queue.c`, `list.c`, `timers.c`, `event_groups.c`) and application demos |

### Key Design Decisions

- **Rust owns fiber lifecycle and scheduling.** C FreeRTOS/Zephyr maintains TCB/thread metadata as auxiliary state kept in sync via bridge functions.
- **One host thread, no async/await.** The simulator runs on a single host thread. All RTOS tasks map to stackful fibers — the C payload expects blocking call stacks.
- **Virtual time, not wall time.** All timers, sleeps, and events are scheduled against a monotonic `u64` tick counter. Wall-clock time is only used for the optional watchdog and host I/O polling.
- **RTOS kernel owns scheduling policy.** costar is the fiber substrate and virtual-time engine; FreeRTOS/Zephyr makes every task-priority and wakeup decision. Documented in `docs/scheduling.md`.

## Cooperative Scheduling with Instrumented Yield Points

The simulator uses a cooperative scheduling model at the fiber/runtime level: simulated RTOS tasks run until they block, sleep, yield, perform certain RTOS operations, or reach an instrumentation checkpoint. This keeps execution deterministic and avoids relying on host threads, host timers, or platform-specific preemption behavior.

For normal RTOS firmware, execution is handed back to the Rust scheduler through the RTOS port layer. Calls such as task delay, queue receive/send, explicit yield, timer waits, and other blocking primitives suspend the active fiber and allow the simulator to advance virtual time or run another task.

For CPU-bound C code, the simulator also supports optional instrumentation-assisted scheduling:

**Tier 1 — Function-entry instrumentation** (`SIM_INSTRUMENT_FUNCTIONS=1`): Compatible C compilers (GCC/Clang) insert `__cyg_profile_func_enter` hooks at every function entry. These call `sim_budget_poll`, which increments a counter and yields the fiber with `BudgetExceeded` if the budget limit is reached.

**Tier 2 — Manual loop hooks** (`SIM_LOOP_POLL()` macro): For tight loops that do not naturally call functions or RTOS primitives, firmware can use `SIM_LOOP_POLL()` from `sim_abi.h`.

**Tier 3 — Edge instrumentation** (`SIM_INSTRUMENT_EDGES=1`): With Clang's `-fsanitize-coverage=trace-pc-guard`, the compiler inserts `__sanitizer_cov_trace_pc_guard` callbacks at every basic-block edge. After a fast thread-local throttle (default: every 10,000 edges), these call `sim_budget_poll`. This is the only tier that can preempt a tight `while(1){}` loop with zero function calls and zero manual checkpoints.

## Features

### RTOS Support
- [x] FreeRTOS: tasks, queues, delays, `vTaskDelayUntil`, critical sections, software timers, semaphores (binary/counting/mutex/recursive), event groups, task notifications, `vTaskDelete`, `xTaskCreateStatic`
- [x] Zephyr: threads, sleeps/timers, `k_sem`, `k_mutex`, `k_msgq`, `k_work`, `k_timer`, `ztest` framework, real kernel via cc crate or west build

### Simulation Engine
- [x] Deterministic min-heap event queue (timestamp → priority → sequence)
- [x] Stackful fibers via `corosensei` with TLS active yielder for C hooks
- [x] Virtual tick interrupt with delayed-task wakeup + tickless idle fast-forward
- [x] Panic boundary: `catch_unwind` catches Rust panics in fibers, marks task Faulted
- [x] 3-tier instrumentation for CPU-bound stall mitigation
- [x] Public `Simulator` API: `run`, `run_until`, `run_until_idle`, `schedule_at`, `cancel`

### Virtual Devices
- [x] UART (trace-backed), GPIO (IRQ-on-change), Timer (one-shot/periodic with IRQ)
- [x] I2C (master-mode, NACK detection), SPI (full-duplex, CPOL/CPHA), CAN (FIFO mailboxes, loopback, error-state)
- [x] ADC (multi-channel, configurable resolution), TempSensor, EEPROM, Flash
- [x] FaultInjector: I2C NACK, SPI corruption, CAN bus error, UART framing error, GPIO stuck-at
- [x] Deterministic entropy source (xorshift128+, seed-based reproducibility)
- [x] IRQ controller with deferred delivery during critical sections
- [x] `inventory`-based compile-time driver registration (sorted init)

### Networking
- [x] Deterministic smoltcp device with rx/tx queues, packet trace
- [x] Smoltcp bridge — deterministic TCP/IP (ARP, ICMP, TCP, UDP) between guest firmware and virtual network
- [x] Host-connected I/O: `polling`-based non-blocking sockets, interactive mode (`--mode interactive`)
- [x] TCP bridge — connect virtual Ethernet device to remote TCP endpoint
- [x] TAP bridge — host-connected Ethernet via `/dev/tapN` (Linux/macOS), raw frame I/O
- [x] FreeRTOS+TCP integrated as git submodule with NetworkInterface V4 driver, `SIM_TCP=1` build
- [x] Ethernet links between machines in World (`[[link]] type = "eth"`)
- [x] Virtual Ethernet device (MAC address, FIFO queues, rx callback, golden trace)
- [x] Networking golden traces: `net` (37 events), `tcp-echo` (22 events, SIM_TCP=1)

### Multi-Machine Simulation (World)
- [x] `World` / `Machine` / `Link` abstractions with shared virtual time
- [x] Deterministic CAN bus topology with broadcast, latency, and fault injection
- [x] Scenario files (TOML): machines, links, buses, packet injections, plants, faults, assertions
- [x] Per-machine RTOS backend selection (FreeRTOS or Zephyr)
- [x] `Firmware` trait for per-machine guest application logic
- [x] `EnvironmentModel` trait for physical plant/physics models
- [x] Scripted BLE event injection (`[[inject]] type = "ble_event"`) with World dispatch
- [x] Ethernet links between machines (`[[link]] type = "eth"`)
- [x] Filesystem image injection (`[[inject]] type = "block_data"`)

### CLI and Tooling
- [x] Subcommand-based CLI: `costar run`, `costar test --all`, `costar shell`, `costar replay`
- [x] JSON-RPC 2.0 server (`costar serve --stdio` / `--bind`) with session management
- [x] Golden trace capture and comparison tests
- [x] JSONL trace output, symbolication, `--diff` comparison, `--machine-filter`
- [x] TOML config file support with serde deserialization
- [x] Board peripheral config mapping (devicetree → virtual devices)
- [x] GDB/LLDB debugging support (docs/debugging.md)
- [x] Go reference client for JSON-RPC protocol (`mcu/`)

## Competitiveness: How Close Are We?

### vs. Zephyr native_sim

| Criterion | Status | Notes |
|-----------|--------|-------|
| Real Zephyr kernel runs natively | **Yes** | cc crate + west build, Linux/macOS |
| Basic kernel primitives | **Yes** | k_sem, k_mutex, k_msgq, k_timer, k_work, k_thread |
| Console/logging | **Yes** | nsi_vprint_trace → stdout |
| ztest pass/fail | **Yes** | Real ztest suites, golden trace in CI (Linux) |
| Cross-platform (Windows) | **Partial** | Zephyr builds via cc crate, but MASM stubs for linker_stubs.S needed |
| 5+ real samples in CI | **No** | ztest demo works; broader Zephyr sample testing not yet automated |
| Upstream Zephyr integration | **No** | External to Zephyr; not a Zephyr board target |
| Zephyr networking stack | **Partial** | Foundation complete: VirtualEthDevice, smoltcp bridge, TCP bridge, TAP bridge, FreeRTOS+TCP integrated. Real Zephyr LwIP compilation pending (~6 days) |
| Zephyr filesystem (littlefs/FAT) | **Partial** | Foundation complete: FlatMemoryStore, C ABI, FreeRTOS demo, block_inject scenario. Real Zephyr littlefs compilation pending (~10 days) |
| Zephyr Bluetooth | **Partial** | Foundation complete: VirtualHciController, BLE scenario DSL, FreeRTOS demo. Real Zephyr BT host compilation pending (~7 days) |
| Zephyr device driver model | **No** | Uses costar C ABI, not Zephyr devicetree driver binding |
| Zephyr logging/settings | **No** | Uses trace events, not Zephyr logging subsystem |
| FreeRTOS+TCP networking | **Yes** | Git submodule, NetworkInterface V4 driver, ARP/ICMP/TCP through smoltcp — unique advantage over native_sim |

**Verdict: ~70% of native_sim coverage.** Strong on kernel primitives, ztest, and cross-platform reach. The subsystem foundation layer (Rust models + C ABI + C drivers + FreeRTOS demos) is complete for networking, filesystem, and Bluetooth — real Zephyr stack compilation is the remaining ~22 days. FreeRTOS+TCP is a unique asset native_sim does not have.

### vs. Renode

| Criterion | Status | Notes |
|-----------|--------|-------|
| Multi-machine simulation | **Yes** | World/Machine/Link, lockstep virtual time |
| Deterministic virtual time | **Yes** | Shared monotonic clock, all machines step in lockstep |
| Scenario files (machines, links, inputs, expectations) | **Yes** | TOML DSL with buses, plants, faults, assertions, BLE events, Ethernet links, block injection |
| Headless CI test runner | **Yes** | `costar test --all` |
| Interactive monitor/debug shell | **Yes** | `costar shell` with run/step/info/trace commands |
| Machine/device-level traces | **Yes** | Tagged with machine ID, RTOS backend; `--machine-filter` |
| JSON-RPC programmatic control | **Yes** | `costar serve` with session management, streaming traces, board config, session cloning, protocol versioning |
| CPU instruction emulation | **No** | Host-native execution only; no ARM/RISC-V emulation |
| Unmodified MCU binaries | **No** | Firmware compiled for host, not cross-compiled for target MCU |
| Memory-mapped peripherals | **No** | Virtual devices are API-based, not MMIO |
| GDB server for remote debugging | **No** | GDB can attach to host process but no remote GDB stub |
| Python scripting API | **No** | JSON-RPC only; no embedded Python REPL |
| Peripheral model library | **Partial** | ~18 device types (UART, GPIO, Timer, IRQ, I2C, SPI, CAN, ADC, TempSensor, EEPROM, Flash, FaultInjector, Entropy, VirtualEthDevice, FlatMemoryStore, VirtualHciController, SmoltcpBridge, TapBridge); Renode has dozens of pre-built platforms |
| Multiple CPU architectures | **No** | Host-native only (x86_64/ARM64 via Rosetta) |
| Co-simulation (SystemC, Verilator) | **Partial** | `EnvironmentModel` trait + plant models for physics co-simulation; no HDL coupling |
| Graphical peripheral viewer | **No** | Terminal-based only |
| .repl/.resc compatibility | **No** | Own TOML format; no Renode platform description compatibility |
| Networking (TCP/IP, Ethernet, TAP) | **Yes** | Smoltcp bridge (deterministic), TCP bridge (host-connected), TAP bridge (host-connected Ethernet, Linux/macOS), FreeRTOS+TCP integrated — competitive or ahead of Renode here |
| Bluetooth | **Partial** | Virtual HCI controller + BLE scenario DSL; real Zephyr BT host pending |

**Verdict: ~65% of Renode-style capability.** Strong on multi-machine orchestration, scenario DSL, CI testing, and JSON-RPC infrastructure. Networking (smoltcp/TCP/TAP bridges + FreeRTOS+TCP) is a standout area. Missing CPU emulation, peripheral breadth, and Renode platform compatibility — but the architecture is intentionally different (host-native, not emulated), so some gaps are by design rather than deficiency.

### Unique Advantages Over Both

| Advantage | Why It Matters |
|-----------|---------------|
| **Cross-platform from day one** | Same binary on Linux, macOS, Windows — no VM, no Docker |
| **Rust-native, no Python** | Single binary with `cargo build`; no Python venv, pip, or CMake dependency |
| **Pure Rust plant models** | `EnvironmentModel` trait enables co-simulation of physical environments alongside firmware |
| **Stackful fibers, not threads** | 5.5M switches/sec, no host thread overhead, true deterministic scheduling |
| **3-tier instrumentation** | Graduated control from cooperative-only through per-edge preemption |
| **Scenario DSL** | TOML-based, human-writable, CI-friendly — easier than Renode's Python .resc |
| **JSON-RPC server** | Lightweight, no dependencies, subprocess-friendly via stdio — ideal for CLI tooling |
| **FreeRTOS+TCP integration** | Real TCP/IP stack compiled into simulator; ARP, ICMP, TCP/UDP through smoltcp bridge — no native_sim or Renode equivalent for FreeRTOS |
| **Dual RTOS support** | FreeRTOS and Zephyr in the same World, per-machine RTOS selection, shared virtual time |

## Subsystem Roadmap (Networking / Filesystem / Bluetooth)

These are the remaining major gaps for full RTOS subsystem support.
Complete designs with API contracts, effort estimates, and integration
patterns are in HANDOFF.md §§25-28.

**Status: Phase 38 foundation layer is COMPLETE.** Rust models (40 unit tests),
C ABI exports (17 functions), C stub drivers (5 files), FreeRTOS golden trace
demos (94 events across 3 modes), smoltcp bridge, TCP bridge, TAP bridge,
FreeRTOS+TCP integration, BLE scenario DSL, Ethernet links, and filesystem
image injection are all done.

**Remaining: ~22 days for Zephyr RTOS stack integration** — compiling
real LwIP, littlefs, and Zephyr BT host via cc crate with Kconfig fragments.

| Subsystem | Effort | Remaining | Key Design |
|-----------|--------|-----------|------------|
| Networking | ~26 days | ~6 days | VirtualEthDevice → smoltcp / TcpBridge / TapBridge + FreeRTOS+TCP done; Zephyr LwIP Kconfig pending |
| Filesystem | ~16 days | ~10 days | FlatMemoryStore + C driver + FreeRTOS demo done; Zephyr littlefs/FAT Kconfig pending |
| Bluetooth | ~14 days | ~7 days | VirtualHciController + BLE DSL + FreeRTOS demo done; Zephyr BT host Kconfig pending |

**Total remaining: ~22 days (~1 month solo, ~2 weeks with two developers).**

## Limitations

1. **Cooperative at fiber level — mitigated by instrumentation.** Tier 3 edge instrumentation covers tight `while(1){}` loops. Without any instrumentation, an infinite loop with no function calls and no RTOS calls can still freeze the simulator.
2. **C undefined behavior is not sandboxed.** The simulator runs firmware in the same process. A wild pointer in C can corrupt the Rust engine. Run sanitizer builds in CI where available.
3. **Host-connected networking is not deterministic.** Host sockets via `polling` are available in interactive mode but are not guaranteed bit-for-bit deterministic.
4. **No CPU instruction emulation.** costar executes firmware natively on the host, not via ARM/RISC-V emulation. It cannot run unmodified MCU binary images.
5. **No process isolation for untrusted firmware.** All simulated tasks share one host process.
6. **Limited Zephyr subsystem support.** Networking, filesystem, Bluetooth, power management, and logging subsystems are not supported.
7. **Scenario golden trace tests have a path resolution bug** — `[expect].trace` paths resolve relative to the scenario directory rather than the project root. Workaround: use absolute paths in scenario files until fixed.
8. **Zephyr Windows build** requires porting `linker_stubs.S` to MASM.

## Supported Platforms

- Linux x86_64 (verified — CI)
- macOS x86_64 (verified)
- macOS Apple Silicon (verified — macOS 26.5.1)
- Windows MSVC x86/x86_64 (verified — CI, Zephyr requires MASM stubs for full kernel build)

CI covers Linux, macOS, and Windows.

## Versioning

costar follows semantic versioning from 1.0.0 forward. The JSON-RPC protocol
version is tracked separately from the crate version — it increments only on
breaking RPC changes. Clients can query the server's protocol version via the
`server.version` RPC method.

- **Crate version**: `costar 1.0.0`
- **Protocol version**: `1`
- **MSRV (Minimum Supported Rust Version)**: 1.84

## Running Tests

```bash
# Full test suite (320 tests)
cargo test --workspace

# Golden trace test (compares output to expected traces — 15 targets, 12 always-pass, 3 conditional)
bash tests/golden_trace_test.sh all

# Scenario golden trace tests (multi-machine simulations — 6 scenarios)
bash tests/scenario_golden_test.sh

# Edge-instrumented tight-loop demo (requires Clang)
SIM_INSTRUMENT_EDGES=1 cargo run -- --mode tight-loop

# Specific crate
cargo test -p sim-core
cargo test -p sim-net
cargo test -p sim-world
```

## Project Structure

```
crates/
  sim-core/          Simulation core (time, event queue, trace, run loop)
  sim-fiber/         Fiber runtime (coroutines, TLS yielder, task states)
  sim-ffi/           C ABI bridge (no_mangle exports, global state, scheduler)
  sim-devices/       Virtual devices (IRQ, timer, UART, GPIO, I2C, SPI, CAN,
                     ADC, TempSensor, EEPROM, Flash, FaultInjector, Entropy,
                     VirtualHciController, FlatMemoryStore, registry)
  sim-net/           Networking (smoltcp device, smoltcp bridge, TCP bridge,
                     TAP bridge, virtual Ethernet device, host poller)
  sim-freertos-port/ FreeRTOS port layer (port.c, portmacro.h, build.rs,
                     sim_coverage.c, sim_hooks.c, sim_kernel_bridge.c,
                     sim_eth.c, sim_block.c, sim_net_if.c,
                     FreeRTOS-Plus-TCP/ submodule)
  sim-zephyr-port/   Zephyr port layer (zephyr_arch.c, thread registry,
                     zephyr_integration/ board definition files, ztest_glue.c)
  sim-runner/        Host binary (main.rs, CLI, JSON-RPC server, shell, replay)
  sim-grpc/          gRPC server (tonic + protobuf) for Electron GUI frontend
                     (session lifecycle, Run stream, display frames, keyframes)
  sim-world/         Multi-machine orchestration (World, Machine, Link, CanBus,
                     scenario DSL, board config, firmware trait, plant models)

c_firmware/
  app/
    main.c                Deterministic demo (queue ping-pong)
    main_interactive.c    Interactive demo (TCP loopback host I/O)
    tight_loop_demo.c     Tier 3 edge-instrumentation demo
    main_broader_api.c    Semaphore, mutex, event group, notification demo
    main_i2c_spi.c        I2C + SPI peripheral exercise
    main_can.c            CAN controller demo
    main_devices.c        Sensor, storage, fault injection demo
    main_entropy.c        Deterministic RNG demo
    main_task_delete.c    vTaskDelete + xTaskCreateStatic demo
    main_net.c            Ethernet frame send/receive demo
    main_block.c          Filesystem block device demo
    main_bt.c             Bluetooth HCI demo
  freertos/               Real FreeRTOS kernel (tasks, queue, list, timers,
                          event_groups)
  zephyr_app/
    standalone_test.c     Zephyr hello-thread demo (no Zephyr SDK)
    standalone_broader_api.c  Simulated broader API test (no real kernel)

mcu/
  client.go               Go JSON-RPC 2.0 client reference implementation

docs/
  HANDOFF.md              Full design document and implementation plan
  IMPLEMENTATION_STATUS.md Per-phase checklist (37 phases)
  scheduling.md           RTOS kernel scheduling ownership doc
  debugging.md            GDB/LLDB integration guide
  costar_requirements.md  Plant model and scenario DSL design decisions

tests/
  golden_trace_test.sh    Golden trace comparator
  scenario_golden_test.sh Multi-machine scenario trace comparator
  scenarios/
    ping_pong.toml        2-machine FIFO link
    three_chain.toml      3-machine cross-traffic chain
    uart_cross.toml       2-machine UART crossover
  traces/
    expected_*.trace      16 golden trace reference files
```

## License

MIT OR Apache-2.0
