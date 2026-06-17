# costar — Implementation Status

Checked items are done and verified. Unchecked items remain for future work.

## Phase 0: Repo and CI
- [x] Workspace skeleton (Cargo.toml, 8 crates)
- [x] `cargo test` passes (83 tests)
- [x] `cargo build` passes (Linux x86_64, macOS, Windows MSVC)
- [x] `cargo fmt --check` passes
- [x] `cargo clippy --all-targets -- -D warnings` passes for all workspace crates
- [x] CI pipeline (.github/workflows/ci.yml — Linux, macOS, Windows)
- [x] Build/test on macOS (Apple Silicon, macOS 26.5.1)
- [x] Build/test on Windows MSVC (build + 83 tests pass; golden trace requires .gitattributes LF enforcement — see `.gitattributes`)

## Phase 1: Event Queue
- [x] EventQueue with deterministic min-heap ordering (timestamp → priority → sequence)
- [x] QueueKey, ScheduledEvent, EventCallback types
- [x] schedule_at / schedule_after / cancel / pop_next / peek_time / is_empty
- [x] Tombstone skipping for cancelled events
- [x] Tests: same-timestamp different priority, same-priority insertion order, cancellation, tombstone, time-rollback detection, deterministic ordering (1,000 events)

## Phase 2: Trace Sink
- [x] TraceSink with TraceEvent enum (EventScheduled, EventDispatched, EventCancelled, TaskResume, TaskYield, InterruptRaised, InterruptDelivered, PacketRx, PacketTx, Fatal, UserU32)
- [x] Human-readable Display impl for each variant
- [x] Golden trace file comparison tests (expected_queue_ping_pong.trace)
- [x] Golden trace test script (tests/golden_trace_test.sh)

## Phase 3: Run Loop
- [x] SimulatorCore: now, queue, running, trace, config
- [x] run() / run_until() / run_until_idle() / stop()
- [x] Time-rollback detection
- [x] Tests: basic run, time-rollback, run_until deadline, cancel event

## Phase 4: Fiber Runtime
- [x] Fiber struct wrapping corosensei Coroutine
- [x] YieldReason and ResumeReason enums (typed coroutine I/O)
- [x] Thread-local active yielder (ACTIVE_YIELDER) for C FFI hooks
- [x] TaskState machine: Created → Running → Suspended/Blocked/Sleeping/Exited/Faulted
- [x] MIN_HOST_COROUTINE_STACK = 64 KiB enforcement
- [x] Single fiber create + run test
- [x] Yield/resume test
- [x] Many-yields stress test (100 yields)
- [x] Sleep/wake test
- [x] Task exit test
- [x] TLS cleared after resume test
- [x] Min stack enforcement test
- [x] Panic propagation test (panics propagate through coroutines, no catch_unwind in MVP)
- [x] 1,000,000 yield/resume stress test (0.18s, ~5.5M switches/s)
- [x] Panic boundary — catch_unwind around fiber resume in scheduler loop, Faulted task state, Fatal trace event
- [x] Panic-caught-and-faulted test (test_fiber_panic_caught_and_faulted)
- [x] Sanitizer builds — ASan/LSan CI job (nightly), `.cargo/config.toml` aliases

## Phase 5: C ABI Header + Rust Exports
- [x] `sim_abi.h` — handwritten stable C ABI header
- [x] `sim_create_task` — register a C task entry point with the Rust fiber runtime
- [x] `sim_start_scheduler` — transfer control to Rust fiber drain loop (with tick-based time advancement)
- [x] `sim_port_yield` — suspend active fiber from C context via TLS yielder
- [x] `sim_task_exit` — mark current task as exited
- [x] `sim_task_delay_until` — suspend active fiber until absolute tick time
- [x] `sim_set_current_task_by_id` — set pxCurrentTCB from Rust scheduler (via sim_kernel_bridge.c mapping)
- [x] `sim_tick_advance` — increment RTOS tick via real FreeRTOS's xTaskIncrementTick()
- [x] `sim_advance_ticks` — batch tick advancement for tickless idle fast-forward
- [x] `sim_enter_critical` / `sim_exit_critical` — thread-local nesting counter
- [x] `sim_trace_u32` — record a u32 data point in the trace
- [x] `sim_now_ticks` — atomic read of current virtual time
- [x] `sim_bridge_register` — register TCB-to-fiber mapping for sim_set_current_task_by_id
- [x] `sim_bridge_add_pending_tcb` — defer TCB fiber creation (for timer daemon, idle tasks)
- [x] `sim_bridge_create_pending_fibers` — lazy fiber creation at scheduler start
- [x] Thread-local RefCell for global state (no deadlock with fiber re-entrancy)
- [x] Thread-local trace buffer for events recorded within fibers
- [x] `sim_exit_critical()` called at scheduler start to balance FreeRTOS's portDISABLE_INTERRUPTS

## Phase 6: FreeRTOS Port Layer
- [x] `port.c` — port implementation for real FreeRTOS (pxPortInitialiseStack, xPortStartScheduler, vPortEndScheduler, vPortYield, pvPortMalloc/vPortFree)
- [x] `portmacro.h` — full port macros for real FreeRTOS (portMAX_DELAY, portYIELD, portENTER_CRITICAL, portDISABLE/ENABLE_INTERRUPTS, portSTACK_GROWTH, etc.)
- [x] `sim_hooks.c` — `__cyg_profile_func_enter` / `__cyg_profile_func_exit` entry points for Tier 1 function-entry instrumentation (weak symbols, calls `sim_budget_poll`)
- [x] `sim_kernel_bridge.c` — TCB-to-fiber mapping array, sim_bridge_register, sim_set_current_task_by_id, pending TCB storage for deferred creation
- [x] `build.rs` — compile C port, bridge, and real FreeRTOS kernel via `cc` crate
- [x] `FreeRTOSConfig.h` — simulator configuration (cooperative, 8 priorities, static+dynamic alloc, 1ms tick, timers enabled)
- [x] pxPortInitialiseStack stores metadata on stack frame (magic, entry, param, handle slot)

## Phase 7: Real FreeRTOS Kernel (C Payload)
- [x] Real FreeRTOS-Kernel from GitHub via Git Submodule (V11.1.0 branch)
- [x] `tasks.c` — full FreeRTOS task management (xTaskCreate, vTaskDelay, vTaskStartScheduler, etc.)
- [x] `queue.c` — full FreeRTOS queue implementation (xQueueCreate, xQueueSend, xQueueReceive)
- [x] `list.c` / `list.h` — real FreeRTOS list operations
- [x] `timers.c` — full FreeRTOS software timers (xTimerCreate, xTimerStart, etc.)
- [x] All required headers: `FreeRTOS.h`, `task.h`, `queue.h`, `list.h`, `timers.h`, `projdefs.h`, `portable.h`, `stack_macros.h`, `deprecated_definitions.h`, `mpu_wrappers.h`
- [x] Minimal tasks.c patches (applied dynamically in `build.rs` during compilation):
  - `#include "sim_abi.h"` for bridge function access
  - `simHandle` field added to TCB struct
  - `vTaskDelay` patched to call `sim_task_delay_until()` before yielding (so Rust scheduler tracks sleep times)
  - `sim_port_task_created` now records TCB in pending list (was no-op)
  - `sim_bridge_create_pending_fibers()` implemented in tasks.c for deferred fiber creation
- [x] Task priority ordering (higher priority scheduled first, round-robin tiebreaker)
- [x] `vTaskDelayUntil` — periodic task scheduling with overflow handling
- [x] `configASSERT` set to no-op to prevent infinite-loop hangs
- [x] Tickless idle optimization — batched tick advancement via `sim_advance_ticks()` (single C↔Rust crossing instead of per-tick loop)
- [x] Software timers — `configUSE_TIMERS=1`, daemon task fiber created via deferred mechanism

## Phase 8: sim-runner Binary
- [x] Host executable linking C firmware + Rust engine
- [x] Calls `c_sim_main()` → creates tasks/queues → creates Rust fibers → registers bridge mappings → `vTaskStartScheduler()` → Rust fiber drain
- [x] Prints trace on completion
- [x] `--golden` CLI flag for machine-readable golden trace output
- [x] `--watchdog <secs>` wall-clock timeout with warning on exceed
- [x] `--help` usage information
- [x] `--mode <deterministic|interactive>` CLI flag — interactive mode initializes HostPoller, runs `c_sim_interactive_main()` (socketpair host I/O demo)
- [x] `--config <path>` CLI flag — TOML config file with serde deserialization (`SimConfig`, `SimulationSection`, `TraceSection`, `deny_unknown_fields`)
- [x] Config file parsing (TOML deserialization) — `SimConfig::from_file()`, 4 unit tests
- [x] Interactive mode implementation — host poller init, scheduler integration, host_poll_and_wake in time-advance path, smart poll timeout bounded by next virtual event

## Phase 9: Two-Task FreeRTOS Demo
- [x] Task A (Sender): sends 5 counter values to queue via xQueueSend, calls vTaskDelay between sends, exits
- [x] Task B (Receiver): receives 5 values from queue via xQueueReceive, calls vTaskDelay when queue empty, exits
- [x] Clean deterministic interleaving with virtual time advancing 0→5 ticks
- [x] 40-event trace (real FreeRTOS queue operations generate additional RtosPortYield events)
- [x] Virtual time advances during delays (tick-based scheduler drives time forward)
- [x] Golden trace test comparing output to expected file (tests/traces/expected_queue_ping_pong.trace, tests/golden_trace_test.sh)
- [x] Rust fibers created from c_sim_main (not trace hook) to avoid coroutine resume crash

## Phase 10: Virtual Devices
- [x] Virtual UART (sim-devices/src/uart.rs — TX/RX buffers, trace-backed writes)
- [x] Virtual GPIO (sim-devices/src/gpio.rs — configurable pins, IRQ-on-change)
- [x] Virtual timer (sim-devices/src/timer.rs — one-shot/periodic, raises IRQs on expiry)
- [x] Interrupt controller (sim-devices/src/irq.rs — pending IRQ tracking, deferred delivery)
- [x] `inventory`-based compile-time driver registration (sim-devices/src/registry.rs)
- [x] C FFI functions added: sim_irq_raise, sim_irq_clear, sim_irq_pending, sim_irq_deliver_pending, sim_uart_write, sim_timer_arm, sim_timer_disarm, sim_gpio_set
- [x] IRQ delivery wired into scheduler loop (post-yield, during tick advance, on critical-section exit)
- [x] Thread-local device storage maps (UART, timer, GPIO) with accessors
- [x] UART write trace test (test_uart_write_trace)
- [x] Timer interrupt wakeup test (test_timer_interrupt_raised)
- [x] Interrupt deferred during critical section test (test_interrupt_deferred_during_critical_section)
- [x] IRQ delivered when not locked test (test_irq_delivered_when_not_locked)

## Phase 11: Networking
- [x] Deterministic smoltcp device (SimNetDevice with rx/tx queues, phy::Device impl)
- [x] Packet capture trace (PacketRx/PacketTx events in trace, via thread-local buffer)
- [x] smoltcp deterministic loopback test (inject → receive → transmit → drain)
- [x] C ABI exports: sim_net_inject_rx, sim_net_drain_tx, sim_net_poll
- [x] Thread-local network device storage (net_device_insert, with_net_device_mut)
- [x] Network injection/drain trace test (test_net_inject_and_drain_traces)
- [x] Host-connected mode — HostPoller with `polling` crate, register/deregister/block/poll
- [x] C ABI for host sockets: sim_host_register_fd, sim_host_deregister_fd, sim_host_block_on_fd
- [x] Scheduler integration: host_poll_and_wake() in idle path when tasks are IoWaiting
- [x] Host poller unit tests (TCP accept, block-wake, deregister, unblock)
- [x] Task blocks on I/O from C firmware — `c_sim_interactive_main()` socketpair demo: Receiver blocks on `sim_host_block_on_fd`, Sender writes, Receiver wakes via host poller

## Phase 12: Zephyr Feasibility
- [x] Design document answering the 7 questions from HANDOFF.md §16 Phase 7 (`docs/zephyr_feasibility.md`)

## Phase 13: Zephyr Hello-Thread POC (Standalone Test)
- [x] `sim-zephyr-port` crate — Rust-side thread registry, scheduler lock, 5 unit tests
- [x] `zephyr_arch.c` / `zephyr_arch.h` — arch port (arch_switch no-op, arch_irq_lock/unlock → sim_enter/exit_critical, arch_k_cycle_get_32 → sim_now_ticks)
- [x] `sim_zephyr_abi.h` — Zephyr-specific C ABI (3-arg thread entry, scheduler lock, sleep)
- [x] `zephyr_glue.c` — convenience wrappers (zephyr_thread_spawn, zephyr_sleep)
- [x] `standalone_test.c` — hello-thread demo (blinker priority 5, worker priority 3, no Zephyr SDK needed)
- [x] Zephyr ABI functions in sim-ffi: `sim_zephyr_init`, `sim_zephyr_register_thread`, `sim_zephyr_set_current_thread`, `sim_zephyr_get_current_thread`, `sim_zephyr_sched_lock`, `sim_zephyr_sched_unlock`
- [x] `sim_zephyr_start_scheduler()` — priority-based scheduler with direct virtual time advancement (no tick counter)
- [x] `--rtos zephyr` CLI flag in sim-runner
- [x] 12-event golden trace (`tests/traces/expected_zephyr_hello.trace`)
- [x] Golden trace tests for both RTOS backends (`bash tests/golden_trace_test.sh all`)
- [x] Zephyr board definition files (Kconfig, DTS, linker.ld, board.cmake) for `west build` — reference files in `crates/sim-zephyr-port/zephyr_integration/`

## Phase 14: Tier 3 Edge Instrumentation (Arbitrary Loop Preemption)
- [x] `sim_coverage.c` — `__sanitizer_cov_trace_pc_guard` callbacks with edge-counter throttle (default 10K edges per budget poll)
- [x] `__sanitizer_cov_trace_pc_guard_init` (no-op for our use case)
- [x] Clang compiler override in `build.rs` when `SIM_INSTRUMENT_EDGES=1` (GCC does not support `trace-pc-guard`)
- [x] `-fsanitize-coverage=trace-pc-guard` applied to ALL C firmware files
- [x] `sim_coverage.c` always compiled (dead code when edge instrumentation is off)
- [x] `--mode tight-loop` CLI flag for Tier 3 demo
- [x] `c_sim_tight_loop_main()` — burner task (5M-iteration tight volatile loop, no function calls) + watchdog task (higher priority, yields cooperatively)
- [x] `sim_budget_set_limit()` C ABI function for runtime budget configuration
- [x] Demo produces 335 events with `SIM_INSTRUMENT_EDGES=1` (151 BudgetExceeded + 10 watchdog_alive interleaved)
- [x] Demo produces 35 events without edge instrumentation (burner runs uninterrupted — tight loop NOT preempted)
- [x] Golden trace: `tests/traces/expected_tight_loop.trace` (335 events, edge-instrumented reference)
- [x] All 83 existing tests pass with edge instrumentation enabled
- [x] `cargo fmt --check` + `cargo clippy` clean

## Phase 15: Broader FreeRTOS API Coverage
- [x] `semphr.h` — standard FreeRTOS semaphore/mutex API header (binary, counting, mutex, recursive mutex)
- [x] `event_groups.c` + `event_groups.h` — event group implementation from FreeRTOS-Kernel
- [x] `FreeRTOSConfig.h` — enabled `configUSE_MUTEXES`, `configUSE_RECURSIVE_MUTEXES`, `configUSE_COUNTING_SEMAPHORES`, `configUSE_TASK_NOTIFICATIONS`, `configUSE_EVENT_GROUPS`
- [x] `main_broader_api.c` — demo exercising: binary semaphore, counting semaphore, mutex, recursive mutex, event group set/wait, task notification send/wait
- [x] `--mode broader-api` CLI flag in sim-runner
- [x] 21-event golden trace (`tests/traces/expected_broader_api.trace`)
- [x] Golden trace test updated to include broader-api (`bash tests/golden_trace_test.sh all`)
- [x] Non-blocking polling pattern (timeout 0 + `vTaskDelay`) for all blocking primitives — no new bridge patches to FreeRTOS kernel needed
- [x] All 83 existing tests pass; `cargo fmt --check` + `cargo clippy` clean

## Known Limitations (per HANDOFF §19)
- [x] Function-entry instrumentation (Tier 1) — `sim_budget_poll`, `BudgetState`, `__cyg_profile_func_enter/exit`, opt-in via `SIM_INSTRUMENT_FUNCTIONS=1`, budget-counter unit test
- [x] Manual loop hooks (Tier 2) — `SIM_LOOP_POLL()` macro in `sim_abi.h`, delegates to `sim_budget_poll()`
- [x] Arbitrary loop preemption (Tier 3 compiler instrumentation) — `-fsanitize-coverage=trace-pc-guard` via Clang, opt-in via `SIM_INSTRUMENT_EDGES=1`, edge-counter throttle, tight-loop demo (`--mode tight-loop`)
- [ ] C UB is not sandboxed (no process isolation)
- [ ] Host-connected networking is not deterministic
- [x] Zephyr hello-thread POC — standalone test with Zephyr-like API (no full Zephyr SDK integration yet)
- [x] README with documented limitations

### Native Rust Task API (§9)
- [x] `TaskContext` — yield_now(), sleep_until(at), sleep_for(delta), now()
- [x] `spawn_rust_task(name, priority, stack_size, f)` — creates stackful fiber from Rust closure
- [x] Panic → Faulted via scheduler's catch_unwind boundary
- [x] Tests: yield→sleep→exit lifecycle, panic isolation

### Public Simulator API (§14)
- [x] `Simulator` struct — wraps `SimulatorCore` (event queue + trace sink)
- [x] `schedule_at` / `schedule_after` / `cancel` — event queue operations
- [x] `run` / `run_until` / `run_until_idle` / `stop` — run-loop control
- [x] `trace()` / `now()` / `is_idle()` — introspection
- [x] 5 tests: schedule+run, run_until, cancel, time-rollback, stop

## Architecture Notes (Real FreeRTOS Integration)

### Fiber Creation Strategy
Rust fibers for tasks created by the user (via `xTaskCreate` from `c_sim_main`)
are created from `c_sim_main` AFTER `xTaskCreate` returns.  Tasks created by
FreeRTOS itself (timer daemon, idle tasks) inside `vTaskStartScheduler()` are
recorded in a pending list via `sim_port_task_created` → `sim_bridge_add_pending_tcb`
and lazily promoted to fibers at the start of `sim_start_scheduler()` via
`sim_bridge_create_pending_fibers()`.  The deferred approach avoids the
segfault that occurs when creating corosensei `Coroutine` objects deep inside
FreeRTOS's call stack.

### Critical Section Bridging
FreeRTOS's `vTaskStartScheduler` calls `portDISABLE_INTERRUPTS()` before `xPortStartScheduler()`. On real hardware, interrupts are re-enabled by the first task's initial stack frame. The simulator balances this with `sim_exit_critical()` at the start of `sim_start_scheduler()`.

### vTaskDelay Bridging
Real FreeRTOS's `vTaskDelay` manipulates internal delayed lists and calls `portYIELD()`. It does not inform the Rust fiber runtime about sleep duration. A patch in `tasks.c` adds a `sim_task_delay_until(xTickCount + xTicksToDelay)` call before the yield so the Rust scheduler can track sleep times and advance virtual time correctly.

### Tick Advance
`sim_tick_advance()` (in `port.c`) calls real FreeRTOS's public `xTaskIncrementTick()` function, which increments `xTickCount` and moves expired delayed tasks to ready lists. `sim_advance_ticks(count)` batches multiple calls for tickless-idle fast-forward (single C↔Rust crossing instead of per-tick loop).

### Panic Boundary
The scheduler wraps `fiber.resume()` in `std::panic::catch_unwind`. A panicking task is marked `TaskState::Faulted`, a `Fatal(PanicCrossedCAbi)` trace event is recorded, and the simulation continues. Resuming a faulted fiber is a no-op.

### SIM_NOW Global Static and Test Concurrency
`SIM_NOW` is a process-wide `static AtomicU64` (not thread-local). This is correct for the simulation — all fibers and the scheduler share the same virtual time. However, in `cargo test` (which runs tests in parallel by default), tests in different threads race on `SIM_NOW`. A test that sets `set_sim_now(200)` can cause a concurrent test's fiber body to read 200 instead of the expected value.

**Mitigation**: Tests that read `SIM_NOW` from within a fiber body must call `set_sim_now(expected)` immediately before the first `task.resume()`, even if `init_global()` already set it — another test thread may have overwritten the value in between.

### Interactive Mode (Host I/O)
When `--mode interactive` is specified:
1. `main.rs` initialises the `HostPoller` (wraps `polling::Poller`) before calling the C entry point.
2. The C firmware (`main_interactive.c`) creates a Unix `socketpair` and two FreeRTOS tasks: Receiver (high priority, blocks on `sim_host_block_on_fd`) and Sender (low priority, writes data after a delay).
3. `sim_host_block_on_fd` reads the current task ID from `CURRENT_TASK_ID` (atomic, avoids RefCell re-entrancy), associates the task with the fd in the poller, and yields with `IoWait`.
4. The scheduler's `host_poll_and_wake()` is called at two points:
   - After time-advance + wake (newly added path)
   - When no sleeping tasks remain (existing fallback path)
5. The poll timeout is bounded by the next virtual event deadline (converted from ticks to wall-clock ms, clamped to [0, 100ms]).
6. When the host fd becomes readable, the blocked task is set `Ready` and resumes on the next scheduler iteration.

The `CURRENT_TASK_ID` atomic is set by the scheduler before fiber resume and cleared after the fiber yields. This allows re-entrant-safe access from within a fiber without touching the global `RefCell<SimGlobal>`.

### Function-Entry Instrumentation (Tier 1 Budget)
When `SIM_INSTRUMENT_FUNCTIONS=1` is set at build time, the C compiler adds `-finstrument-functions`, which emits calls to `__cyg_profile_func_enter` at every C function entry.  The hook (defined in `sim_hooks.c`) calls `sim_budget_poll()`, which:
1. Increments a thread-local entry counter (`BudgetState::entry_count`).
2. If the counter reaches `max_entries` (default 1,000,000), sets the `exceeded` flag.
3. If inside a fiber (`has_active_fiber()`), resets the counter and yields with `BudgetExceeded`.  The fiber resumes from the instruction after the yield with a fresh budget.
4. If outside a fiber (unit test), leaves the exceeded state for inspection and returns normally.

- [x] All 83 existing tests pass; `cargo fmt --check` + `cargo clippy` clean

## Phase 16: Real Zephyr Integration (west build + cc crate compilation)

### Approach — two modes

**Mode A: west build (Linux/macOS)**
Uses the existing `native_sim/native/64` board. `west build` produces `zephyr.elf` —
a relocatable partial link with 50 undefined runner symbols. We provide those symbols
in Rust (`zephyr_glue.rs`) and C (`nsi_shim.c`). `sim-runner/build.rs` localizes Zephyr's
`main` symbol via `objcopy` and links `zephyr.elf`.

**Mode B: cc crate compilation (cross-platform — Linux, macOS, Windows)**
Compiles ~40 real Zephyr kernel source files directly via the `cc` crate in
`sim-zephyr-port/build.rs`. No `west`, no CMake, no Kconfig, no Python codegen.
Pre-generated config headers (autoconf.h, offsets.h, devicetree_generated.h,
79 syscall stubs) are checked into `config/zephyr/` — they were generated once
from a hello_world `west build` and never need regeneration.

The arch layer (`sim_arch.c`) replaces `arch/posix/core/{swap,thread,posix_core_nsi}.c`
and maps `arch_swap` → `nct_swap_threads` → corosensei fiber yield. 13 linker
section symbols (init markers, device list, ctor array) that Zephyr's custom
linker script normally provides are stubbed in `linker_stubs.S` — all aliases to
a single zero address so empty-section iteration is a no-op.

| Category | Symbols | Implementation |
|----------|---------|----------------|
| NCT (thread emulation) | `nct_init`, `nct_new_thread`, `nct_swap_threads`, `nct_first_thread_start`, etc. (8) | Rust: multi-fiber — `nct_new_thread` creates per-thread `Fiber`, `nct_swap_threads` yields current fiber + signals next to drain loop |
| NCE (CPU emulator) | `nce_init`, `nce_boot_cpu`, `nce_halt_cpu`, etc. (5) | Rust: boot calls `z_cstart()`, halt yields fiber |
| HW models | `hw_irq_ctrl_*`, `hwtimer_*` (18) | Rust: stubs (hello_world doesn't use hardware) |
| NSI (simulator interface) | `nsi_exit`, `nsi_vprint_*`, `nsi_simu_time`, etc. (9) | C: proper `va_list` handling for vfprintf |
| Libc | `snprintf`, `strcmp`, `strlen`, `__stack_chk_fail`, etc. (10) | Host libc |

### Build Flow

```bash
# Mode A: west build (Linux, requires Zephyr SDK + west workspace)
cd zephyr-workspace/
west build -b native_sim/native/64 zephyr/samples/hello_world
cd universal-rtos-native-simulator/
ZEPHYR_BUILD_DIR=../zephyr-workspace/build cargo run --features zephyr_real -- --rtos zephyr

# Mode B: cc crate (cross-platform — Linux, macOS, Windows — no west/SDK needed)
# Requires Zephyr source tree at the location specified by ZEPHYR_BASE.
ZEPHYR_BASE=../zephyr-workspace/zephyr cargo run --features zephyr_real -- --rtos zephyr
```

### Result

```
Hello World! native_sim/native/64
Zephyr ERROR: CODE_UNREACHABLE reached from .../thread_entry.c:57
```

The `CODE_UNREACHABLE` is expected: Zephyr's `main()` returns, triggering `k_thread_abort`.
Exit code is 0.

### Architecture

**Multi-fiber, cooperative switching** — shared by both modes.

Each Zephyr thread gets its own corosensei fiber (via `nct_new_thread`). The
boot fiber runs `z_cstart()` → Zephyr init → scheduler. When Zephyr's `arch_swap()`
calls `nct_swap_threads()`, the current fiber yields with `next_to_resume` set to
the next thread ID. The Rust drain loop (in `main.rs`) takes the next thread's
fiber via `nct_take_fiber()`, resumes it on its own corosensei stack, and returns
it via `nct_return_fiber()`.  This continues until all Zephyr threads exit.

Fiber storage uses a heap-allocated `Vec<Option<Fiber>>` managed via raw pointer
inside `NctState`.  The `Option::take()` pattern avoids re-entrant borrow issues
when a fiber calls back into `nct_swap_threads` during yield.

**Mode B (cc crate) additional detail**: `sim-zephyr-port/build.rs` compiles
the Zephyr kernel, arch layer, board files, and drivers directly via the `cc`
crate. When `ZEPHYR_BASE` is set, this produces `embedded_zephyr_payload`
(which includes `posix_boot_cpu`). When `ZEPHYR_BASE` is not set, it compiles
the standalone test instead. `sim-runner/build.rs` detects `ZEPHYR_BASE` and
sets `cfg(zephyr_cc_kernel)` to gate the real kernel code path.

### Files

| File | Purpose |
|------|---------|
| `crates/sim-runner/build.rs` | Detects `ZEPHYR_BUILD_DIR` (west build) or `ZEPHYR_BASE` (cc crate), sets `cfg(zephyr_linked)` / `cfg(zephyr_cc_kernel)`, links `zephyr.elf` when applicable |
| `crates/sim-runner/src/zephyr_glue.rs` | Rust `#[no_mangle]` exports for nct_*, nce_*, hw_* (used by both modes) |
| `crates/sim-zephyr-port/c/nsi_shim.c` | C implementations for nsi_vprint_*, nsi_exit, nsi_simu_time (used by both modes) |
| `crates/sim-zephyr-port/c/sim_arch.c` | Arch layer: replaces `arch/posix/core/{swap,thread,posix_core_nsi}.c` — maps Zephyr's posix arch to nct_* corosensei functions |
| `crates/sim-zephyr-port/c/linker_stubs.S` | Assembly aliases for Zephyr's linker-script section symbols (init markers, device list, ctor array) |
| `crates/sim-zephyr-port/build.rs` | Dual-mode: standalone test (no `ZEPHYR_BASE`) or real kernel compilation via cc crate (40+ Zephyr source files) |
| `crates/sim-zephyr-port/config/` | Pre-generated Zephyr config headers (autoconf.h, offsets.h, devicetree_generated.h, 79 syscall stubs, configs.c) |
| `crates/sim-runner/Cargo.toml` | `zephyr_real` feature, `check-cfg` for `zephyr_linked` and `zephyr_cc_kernel` |

### Known Limitations

- [x] `main()` return triggers `CODE_UNREACHABLE` (expected — Zephyr threads shouldn't return)
- [x] Only host compiler (no Zephyr SDK cross-compiler needed for native_sim)
- [x] Virtual time synchronized with `nsi_simu_time` — Rust sets it before each fiber resume, Zephyr reads via `arch_k_cycle_get_32` → `nsi_simu_time`
- [x] Console output via `nsi_vprint_trace` → `vfprintf(stdout)` with `fflush`; stdout set to unbuffered via constructor
- [x] `nsi_vprint_error_and_exit` → `_exit(0)` for clean process termination (switched from `exit` to `_exit` in Phase 17 to avoid Rust destructor panics on coroutine stack)
- [x] Real Zephyr kernel compiles via cc crate (Mode B) — cross-platform: works anywhere `cc` crate works (Linux, macOS, Windows MSVC); no west/CMake/Kconfig/DTS needed at build time
- [x] Multi-threaded Zephyr apps: each Zephyr thread gets its own corosensei fiber via `nct_new_thread`; `nct_swap_threads` yields current fiber and signals next thread to drain loop
- [x] `main` symbol conflict in cc crate build: Zephyr's `bg_thread_main` → `main()` resolved to Rust's `main()` — fixed by compiling `init.c` separately with `-Dmain=zephyr_app_main`
- [x] `ztest` integration — builds + runs real Zephyr test suites via cc crate with ELF linker section fragment (ztest_sections.ld), golden trace test in CI
- [ ] Zephyr build not yet in CI pipeline (though `zephyr-real-check` compiles the feature-gated Rust code)
- [ ] Mode B tested on Linux only; macOS expected to work via Clang; Windows MSVC needs linker_stubs.S ported to MASM
- [ ] Mode B uses app from `crates/sim-zephyr-port/config/app_main.c`; custom apps need their own main.c compiled separately
- [ ] ztest linker section fragment (ztest_sections.ld) is Linux-only (GNU ld / lld `INSERT AFTER` directive); macOS Mach-O and Windows MSVC linkers not supported for ztest mode

### Phase 17: Multi-Fiber Zephyr (Real Kernel Integration)

- [x] `zephyr_glue.rs`: NctState extended with `Vec<Option<Fiber>>` (heap-allocated), `nct_new_thread` creates per-thread fibers, `nct_first_thread_start`/`nct_swap_threads` signal `next_to_resume`
- [x] `main.rs`: `run_zephyr_real()` multi-fiber drain loop — takes next fiber via `nct_take_fiber()`, resumes it with panic boundary, returns it via `nct_return_fiber()`
- [x] `Option::take()` pattern avoids re-entrant borrow when fiber calls `nct_swap_threads`
- [x] `nsi_shim.c`: `exit(0)` → `_exit(0)` to avoid Rust destructor panics on coroutine stack
- [x] `build.rs`: `init.c` compiled separately with `-Dmain=zephyr_app_main` to avoid symbol collision with Rust's `main()` ELF entry point
- [x] `config/app_main.c`: multi-threaded Zephyr test app (two threads with `k_sleep`/`k_yield` + `sim_trace_u32` events)
- [x] `../sim-ffi/include` added to cc crate include path for `sim_abi.h` access
- [x] All 83 existing tests pass; golden traces pass; `cargo fmt --check` + `cargo clippy` clean

### Phase 17b: Virtual Time Advancement + Timer Driver

- [x] `sim_arch.c`: replaced `native_posix_timer.c` — provides `sys_clock_set_timeout` hook (records delta ticks in `g_rtos_ticks_until_wake`), `sys_clock_cycle_get_32/64` (reads `nsi_simu_time`), `sys_clock_elapsed`, `sys_clock_disable`, `sys_clock_driver_init`
- [x] `sim_arch.c`: `sim_clock_announce()` wrapper calls kernel's `sys_clock_announce()` to process expired timeouts
- [x] `main.rs`: drain loop advances virtual time to next Zephyr timeout deadline before resuming fibers — reads `g_rtos_ticks_until_wake`, converts ticks→cycles (10,000:1), calls `sim_clock_announce()`
- [x] `build.rs`: `native_posix_timer.c` excluded; timer functions provided by `sim_arch.c`
- [x] Eliminated `sim_time += 1` drift — time now advances in proper tick-sized jumps synchronized with Zephyr's timeout queue

### Phase 18: Peripheral Event Queue (RTOS-Agnostic)

- [x] `sim-ffi/src/lib.rs`: thread-local `EVENT_QUEUE` (BTreeMap<u64, Vec<C-callback>>) owned by the costar engine, not any RTOS
- [x] `sim_schedule_event(at_cycles, callback)` C ABI — virtual devices (UART, timer, GPIO) schedule C callbacks that fire at the right virtual time
- [x] `next_event_deadline()` / `dispatch_events(now)` — drain loop queries the queue alongside RTOS timeout
- [x] `main.rs`: drain loop advances to `min(rtos_timeout_deadline, event_deadline)` — peripherals keep pace with CPU
- [x] `sim_abi.h`: `sim_schedule_event` declaration documented as RTOS-agnostic
- [x] `config/app_main.c`: virtual timer peripheral demo — schedules `vtimer_callback` at +10,000 cycles that calls `sim_trace_u32` + `sim_irq_raise(5)`

### Phase 18b: FreeRTOS Event Queue Integration

- [x] `sim-ffi/src/lib.rs` FreeRTOS scheduler loop: before advancing to next RTOS wake time, checks `next_event_deadline()`. If a peripheral event fires sooner, advances to the event deadline, calls `dispatch_events()` and `deliver_pending_irqs()`, then proceeds to the RTOS timeout.
- [x] `dispatch_events()` called after each time advancement in the FreeRTOS tickless idle path — same pattern as the Zephyr drain loop.
- [x] Feature parity achieved: peripherals keep pace with CPU threads identically for both FreeRTOS and Zephyr.

### Phase 18c: Zephyr Broader RTOS API Coverage

- [x] `config/app_broader_api.c` — exercises k_sem, k_mutex, k_msgq, k_timer, k_work against real Zephyr kernel via cc crate build
- [x] `build.rs` — supports `ZEPHYR_APP=broader_api` to select the broader API demo app
- [x] `sim_arch.c` — fixed `sys_clock_set_timeout` idle guard to prevent idle thread from overwriting legitimate timeout values; added `ticks > 1000000` filter for INT32_MAX sentinel values
- [x] `sim_arch.c` — added `sim_get_ready_thread_id()` to query Zephyr's ready queue via `z_swap_next_thread()`, replacing broken `z_reschedule_irqlock` approach
- [x] `sim_arch.c` — fixed `z_impl_k_thread_abort` to call Zephyr's internal `z_thread_abort()` for proper thread cleanup (removes from ready queue, cancels timeouts, marks DEAD), then triggers reschedule
- [x] `sim_arch.c` — stub for `z_fatal_error` (called when essential thread is aborted)
- [x] `main.rs` — drain loop calls `sim_get_ready_thread_id()` + `nct_signal_next()` after `sim_clock_announce` to manually direct scheduler to newly-ready thread
- [x] `main.rs` — allowed `--mode broader-api` with `--rtos zephyr` (was previously blocked)
- [x] `nsi_shim.c` — added `flush_trace_pending()` call before `_exit()` so trace events from within fibers are printed
- [x] `sim-ffi/src/lib.rs` — added `flush_trace_pending()` `#[no_mangle]` function
- [x] `tests/traces/expected_zephyr_broader_api.trace` — golden trace with 18 events
- [x] `tests/zephyr_broader_api_golden_test.sh` — standalone golden trace test script
- [x] `tests/golden_trace_test.sh` — added `zephyr-broader-api` target, skips if `ZEPHYR_BASE` unset
- [x] `.github/workflows/ci.yml` — Zephyr broader API golden trace test step (Linux only, requires Zephyr source)
- [x] `crates/sim-runner/build.rs` — gates `cfg(zephyr_cc_kernel)` on `CARGO_FEATURE_ZEPHYR_REAL` to prevent mismatch

### Phase 19: Zephyr Ztest Integration

- [x] `crates/sim-zephyr-port/c/ztest_glue.c` — non-inline wrappers for ztest static-inline functions (ztest_run_test_suites, __ztest_set_test_result/phase)
- [x] `crates/sim-zephyr-port/c/ztest_sections.ld` — GNU ld linker script fragment that groups `._ztest_*.static.*` subsections and provides `_ztest_*_list_start` / `_list_end` symbols via `INSERT AFTER .data`
- [x] `crates/sim-zephyr-port/config/app_ztest.c` — ztest demo app with `costar_suite` (test_sem_give_take, test_mutex_lock_unlock, test_msgq_put_get) using `ZTEST` / `ZTEST_SUITE` macros
- [x] `crates/sim-zephyr-port/config/zephyr/syscalls/ztest_test.h` — empty syscall stub for ztest_test.h
- [x] `crates/sim-zephyr-port/config/zephyr/autoconf.h` — added `CONFIG_ZTEST=y`, `CONFIG_ZTEST_NEW_API=y`, `CONFIG_ZTEST_FATAL_HOOK=y`
- [x] `crates/sim-zephyr-port/build.rs` — added ztest subsystem compilation (ztest.c renamed to `zephyr_ztest_main`, ztest_defaults.c, ztest_glue.c), GNU ld fragment injection when `ZEPHYR_APP=ztest` on Linux
- [x] `crates/sim-zephyr-port/c/linker_stubs.S` — comment documenting that ztest section markers come from `ztest_sections.ld` (INSERT AFTER .data), not from the same-address stubs
- [x] `crates/sim-runner/build.rs` — emits `cargo:rustc-link-arg=-Wl,-T,.../ztest_sections.ld` when `ZEPHYR_APP=ztest` on Linux
- [x] `crates/sim-runner/src/main.rs` — added `SimMode::Ztest` variant, CLI parsing (`--mode ztest`), config file support, validation (requires `--rtos zephyr` + `zephyr_real` feature)
- [x] `c_firmware/zephyr_app/standalone_test.c` — added peripheral event queue exercises: virtual timer callback via `sim_schedule_event`, deferred work callback, blinker sleep extended to 5 ticks
- [x] `c_firmware/zephyr_app/standalone_broader_api.c` — standalone broader API test (simulated sem/mutex/msgq/timer/work without real Zephyr kernel), selectable via `ZEPHYR_APP=broader_api`
- [x] `tests/traces/expected_zephyr_hello.trace` — updated for vtimer_fired + deferred_work_done events, blinker sleep extended to 5
- [x] `tests/traces/expected_zephyr_ztest.trace` — golden trace with 7 events (ztest_main_start, 3 test pairs: msgq, mutex, sem)
- [x] `tests/golden_trace_test.sh` — added `zephyr-ztest` target (skips if `ZEPHYR_BASE` unset), integrated into `all`
- [x] `crates/sim-zephyr-port/build.rs` — clippy fix: removed unnecessary `&` from `ztest_dir.join("include")` calls
- [x] All 83 existing tests pass; golden traces pass; `cargo fmt --check` + `cargo clippy` clean

### Scheduling Architecture Documentation

- [x] `docs/scheduling.md` — documents the scheduling ownership split: the RTOS kernel (FreeRTOS or Zephyr) makes every scheduling decision; costar is the fiber substrate and virtual-time engine. Covers preemption caveat, peripheral event flow, and which components each side owns.

### Phase 20: Multi-Node Simulation (World / Machine / Link)

- [x] `crates/sim-world/` — new crate with World, Machine, and Link abstractions
- [x] `crates/sim-world/src/link.rs` — deterministic FIFO channel between machines with configurable latency, `send()` / `drain_arrived()` / `next_arrival_time()` API, 4 unit tests
- [x] `crates/sim-world/src/machine.rs` — `Machine` wraps `Simulator` with machine-ID-tagged traces, `schedule_at()` / `spawn_rust_task()` / `advance_to()` / `next_event_time()` API, 6 unit tests
- [x] `crates/sim-world/src/world.rs` — `World` global event loop: shared virtual clock, lockstep machine advancement, link delivery with PacketRx trace recording, `run()` / `run_until()` / `stop()`, 7 unit tests
- [x] `crates/sim-ffi/src/simulator.rs` — added `peek_time()` and `record_trace()` methods for multi-machine integration
- [x] `Cargo.toml` — added `sim-world` to workspace members
- [x] All 100 tests pass (83 existing + 17 sim-world + 1 doctest); `cargo fmt --check` + `cargo clippy` clean

### Phase 21: Scenario Files

- [x] `crates/sim-world/src/scenario.rs` — TOML scenario file parsing (serde), validation (duplicate IDs, missing links, unknown fields), and execution via `Scenario::run()` that builds a World, pre-loads packet injections into links, runs the simulation, and compares against expected golden traces
- [x] `crates/sim-world/Cargo.toml` — added `serde`, `toml`, and `thiserror` workspace dependencies
- [x] `crates/sim-world/src/lib.rs` — exported `scenario` module and types (`Scenario`, `ScenarioError`, `ScenarioResult`)
- [x] `crates/sim-world/src/world.rs` — added `inject_packet(from, to, data, at)` method for scenario-based packet injection
- [x] Scenario file format: `[[machine]]`, `[[link]]`, `[[inject]]`, `[expect]` sections with validation (duplicate IDs, unknown machine refs, unknown link refs)
- [x] `crates/sim-runner/Cargo.toml` — added `sim-world` dependency
- [x] `crates/sim-runner/src/main.rs` — added `--scenario <path>` CLI flag with `run_scenario()` function
- [x] `tests/scenarios/ping_pong.toml` — 2-machine unidirectional ping-pong scenario (1 link, 1 injection, golden trace)
- [x] `tests/scenarios/three_chain.toml` — 3-machine cross-traffic chain scenario (3 links, 3 injections, golden trace)
- [x] `tests/scenario_golden_test.sh` — CI-compatible golden trace test script for scenarios (strip CR, diff, pass/fail counters)
- [x] Both scenario golden traces match expected output
- [x] All 109 unit tests pass (83 existing + 17 sim-world + 9 scenario); 2 scenario golden traces pass; `cargo fmt --check` + `cargo clippy` clean

### Known Limitations (Scenario Files)

### Phase 22: Virtual I2C and SPI Devices

- [x] `crates/sim-devices/src/i2c.rs` — `VirtualI2c` master-mode controller (TX/RX buffers, address selection, NACK detection, write/read/write_read, 9 unit tests)
- [x] `crates/sim-devices/src/spi.rs` — `VirtualSpi` master-mode controller (full-duplex transfer, CPOL/CPHA mode config, CS management, word size, 11 unit tests)
- [x] `crates/sim-devices/src/lib.rs` — thread-local I2C + SPI storage maps, insert/accessor helpers (`i2c_insert`, `with_i2c_mut`, `spi_insert`, `with_spi_mut`)
- [x] `crates/sim-ffi/src/lib.rs` — C ABI exports: `sim_i2c_write`, `sim_i2c_read`, `sim_i2c_write_read`, `sim_i2c_set_address`, `sim_i2c_get_nack`, `sim_i2c_inject_rx`, `sim_spi_transfer`, `sim_spi_set_config`, `sim_spi_set_cs`, `sim_spi_inject_rx`
- [x] `crates/sim-ffi/include/sim_abi.h` — C ABI declarations for all I2C and SPI functions
- [x] `c_firmware/app/main_i2c_spi.c` — C demo: Task A exercises I2C (write 3 bytes, read 4 bytes, write_read 2 bytes, NACK check), Task B exercises SPI (config Mode0→Mode3, CS control, full-duplex transfer, write-only)
- [x] `crates/sim-runner/src/main.rs` — `--mode i2c-spi` CLI flag, `SimMode::I2cSpi` variant, device initialization before C entry point
- [x] `crates/sim-freertos-port/build.rs` — added `main_i2c_spi.c` to C compilation
- [x] `tests/traces/expected_i2c_spi.trace` — 22-event golden trace
- [x] `tests/golden_trace_test.sh` — `i2c-spi` target and `all` integration
- [x] All 131 unit tests pass; all golden traces pass; `cargo fmt --check` + `cargo clippy` clean

### Phase 23: Virtual CAN Bus Device

- [x] `crates/sim-devices/src/can.rs` — `VirtualCan` controller with TX/RX FIFO mailboxes, standard/extended ID, data/remote frames, loopback mode, error-state tracking (ErrorActive/Warning/Passive/BusOff), 9 unit tests
- [x] `crates/sim-devices/src/lib.rs` — thread-local CAN storage map, `can_insert`/`with_can_mut`/`with_can` accessors
- [x] `crates/sim-ffi/src/lib.rs` — C ABI exports: `sim_can_send`, `sim_can_recv`, `sim_can_inject_rx`, `sim_can_set_loopback`, `sim_can_get_error`
- [x] `crates/sim-ffi/include/sim_abi.h` — C ABI declarations for all CAN functions
- [x] `c_firmware/app/main_can.c` — C demo: Task A (high priority) sends 3 CAN frames (std data, ext data, RTR), injects an external frame, checks error state; Task B (low priority) receives all 4 frames in loopback mode, verifies IDs/fields/data, then checks empty queue
- [x] `crates/sim-runner/src/main.rs` — `--mode can` CLI flag, `SimMode::Can` variant, CAN device initialization before C entry point
- [x] `crates/sim-freertos-port/build.rs` — added `main_can.c` to C compilation
- [x] `tests/traces/expected_can.trace` — 37-event golden trace
- [x] `tests/golden_trace_test.sh` — `can` target and `all` integration
- [x] All 140 unit tests pass; all golden traces pass; `cargo fmt --check` + `cargo clippy` clean

### Phase 24: Platform/Device Ecosystem — Sensors, Storage, Fault Injection

- [x] `crates/sim-devices/src/sensor.rs` — `VirtualAdc` (multi-channel ADC, configurable resolution 8/10/12/16-bit, per-channel injected readings, reference voltage) and `VirtualTempSensor` (temperature in millidegrees Celsius), 12 unit tests
- [x] `crates/sim-devices/src/storage.rs` — `VirtualEeprom` (byte-addressable, write-count tracking, up to 64KB) and `VirtualFlash` (page-addressed, erase-before-write, per-page erase counts), 14 unit tests
- [x] `crates/sim-devices/src/fault.rs` — `FaultInjector` with I2C NACK, SPI data corruption, CAN bus error, UART framing error, GPIO stuck-at faults; one-shot consume semantics, 8 unit tests
- [x] `crates/sim-devices/src/lib.rs` — thread-local storage maps (ADCS, TEMP_SENSORS, EEPROMS, FLASHES, FAULT_INJECTOR) + insert/accessor helpers + C ABI exports for storage and fault injection
- [x] `crates/sim-ffi/src/lib.rs` — C ABI exports for sensors (sim_adc_read, sim_adc_inject_reading, sim_adc_set_resolution, sim_temp_read, sim_temp_set_value); fault injection integrated into sim_i2c_read (NACK→return 0) and sim_spi_transfer (error→corrupt byte)
- [x] `crates/sim-ffi/include/sim_abi.h` — C ABI declarations for all sensor, storage, and fault injection functions
- [x] `c_firmware/app/main_devices.c` — combined FreeRTOS demo: ADC read (2048), temperature read (30.5°C), EEPROM write/read (0xAA/0x55), Flash erase/write/read (0xDEADBEEF), I2C NACK fault inject, fault clear
- [x] `crates/sim-runner/src/main.rs` — `--mode devices` CLI flag, `SimMode::Devices` variant, device initialization (ADC, temp sensor, EEPROM, flash)
- [x] `crates/sim-freertos-port/build.rs` — added `main_devices.c` to C compilation
- [x] `tests/traces/expected_devices.trace` — 19-event golden trace
- [x] `tests/golden_trace_test.sh` — `devices` target and `all` integration
- [x] All 174 unit tests pass; all golden traces pass; `cargo fmt --check` + `cargo clippy` clean

### Phase 25: JSONL Traces and CLI Improvements

- [x] `crates/sim-core/Cargo.toml` — added `serde`, `serde_json` dependencies
- [x] `crates/sim-core/src/trace.rs` — `TraceEvent` derives `serde::Serialize` with `#[serde(tag = "event")]` for self-describing JSONL; `SimErrorCode` derives `Serialize + Deserialize`; `TraceSink::to_jsonl()` and `TraceSink::write_jsonl()` methods
- [x] 5 new unit tests: JSONL serialization, multi-event JSONL output, write-to-writer, backward-compat with human format, fatal event serialization
- [x] `crates/sim-runner/src/main.rs` — `TraceFormat` enum (`Human`/`Jsonl`), `--trace-format <human|jsonl>` CLI flag, `--list-modes` flag, `--verbose` flag, `--diff <path>` flag for trace comparison
- [x] `crates/sim-runner/src/config.rs` — `TraceSection::format` field for config-file-driven trace format selection
- [x] `costar trace diff` functionality via `--diff <path>` — compares trace output against expected file, exits 0 on match, 1 on mismatch
- [x] Backward-compatible: human-readable golden trace format unchanged; `--golden` and `--trace-format human` produce identical output
- [x] All 180 unit tests pass (174 existing + 5 trace + 1 config extension); `cargo fmt --check` + `cargo clippy` clean

### Phase 26: Headless Test Runner (Subcommand CLI)

- [x] Subcommand-based CLI: `costar run [OPTIONS]` (default), `costar test [SCENARIOS...] [OPTIONS]`, `costar shell [SCENARIO]` (interactive monitor)
- [x] `costar test --all` — auto-discovers and runs all `tests/scenarios/*.toml` files
- [x] `costar test --list` — lists discoverable scenario tests
- [x] `costar test <path>` — run single scenario with automatic golden trace comparison
- [x] `costar test <path>... --verbose` — run multiple scenarios with per-test PASS/FAIL output
- [x] Exit code 0 on all pass, 1 on any failure — CI-ready
- [x] Backward-compatible: no subcommand defaults to `run` behavior; all existing flags unchanged
- [x] `costar shell` interactive monitor now functional (Phase 27)
- [x] `costar test --help` / `costar run --help` / `costar --help` — self-documenting
- [x] All 186 unit tests pass; all golden traces pass; `cargo fmt --check` + `cargo clippy` clean

### Phase 28: Debugging and Tracing

- [x] `TraceEvent::TaskCreated` variant — records task name at creation time for symbolication
- [x] `TraceSink::resolve_task_name()` / `task_symbols()` / `format_symbolicated()` — trace-level symbol resolution
- [x] `sim_register_symbol(task_id, name)` C ABI — manual symbol registration from C
- [x] `sim_create_task` auto-emits TaskCreated event
- [x] `--symbolicate` CLI flag — prints trace with resolved task names
- [x] `costar replay <trace.jsonl> [--step]` subcommand — replay a trace file with symbolication, step-through mode
- [x] `docs/debugging.md` — GDB/LLDB integration, crash investigation, simulation hang diagnosis, sanitizer docs
- [x] `serde_json` dependency added to sim-runner for replay subcommand
- [x] All golden traces regenerated to include TaskCreated events
- [x] 186 unit tests pass; all golden traces pass; `cargo fmt --check` + `cargo clippy` clean

### Phase 29: Cross-Platform Hardening

- [x] Replaced POSIX `socketpair` with TCP loopback in `main_interactive.c` (cross-platform)
- [x] Added `tcp_loopback_pair()` helper — creates connected non-blocking TCP sockets via 127.0.0.1
- [x] Platform abstraction: `#ifdef _WIN32` for Winsock2 (WSAStartup, closesocket, ioctlsocket) vs POSIX (fcntl, close)
- [x] Replaced `read()`/`write()` with `recv()`/`send()` for socket I/O portability
- [x] Removed `cfg(not(windows))` gating from build.rs — `main_interactive.c` compiles everywhere
- [x] Removed `#[cfg(windows)]` error exit from main.rs extern declaration (C code is cross-platform)
- [x] Interactive mode still gated for Windows at runtime (host poller uses std::os::fd, Unix-only)
- [x] Updated `--list-modes` text: "TCP loopback" instead of "socketpair"
- [x] 186 unit tests pass; all golden traces pass; `cargo fmt --check` + `cargo clippy` clean

### Phase 30: Virtual Entropy Device

- [x] `crates/sim-devices/src/entropy.rs` — `VirtualEntropy` deterministic pseudo-random number generator backed by xorshift128+; seed-based determinism (same seed → same output), `request_bytes(buf)`, `seed(val)`, `reset()`, 9 unit tests
- [x] `crates/sim-devices/src/lib.rs` — thread-local `ENTROPY_SOURCES` storage map, `entropy_insert`/`with_entropy_mut`/`with_entropy` accessors
- [x] `crates/sim-ffi/src/lib.rs` — C ABI exports: `sim_entropy_request(id, buf_ptr, len)`, `sim_entropy_seed(id, seed)`
- [x] `crates/sim-ffi/include/sim_abi.h` — C ABI declarations for entropy functions
- [x] `c_firmware/app/main_entropy.c` — C demo: Collector task requests 8 bytes (default seed), reseeds with `0xDEADBEEFCAFEBABE`, requests 8 more, verifies determinism (same seed → same output); Observer task waits on queue for notification
- [x] `crates/sim-runner/src/main.rs` — `--mode entropy` CLI flag, `SimMode::Entropy` variant, entropy device initialization before C entry point
- [x] `crates/sim-freertos-port/build.rs` — added `main_entropy.c` to C compilation
- [x] `tests/traces/expected_entropy.trace` — 18-event golden trace
- [x] `tests/golden_trace_test.sh` — `entropy` target and `all` integration
- [x] All 195 unit tests pass; all golden traces pass; `cargo fmt --check` + `cargo clippy` clean

### Phase 31: FreeRTOS Task Deletion and Static Allocation

- [x] `sim-ffi/src/lib.rs` — `PENDING_DELETIONS` thread-local, `sim_task_deleted()` C ABI, `process_pending_deletions()` integration in scheduler loop (after yield + before runnable search)
- [x] `sim-fiber/src/task.rs` — `Fiber::mark_deleted()` method: sets TaskState::Exited, takes coroutine via `ManuallyDrop` to avoid force-unwind crash
- [x] `sim-ffi/include/sim_abi.h` — `sim_task_deleted` and `sim_bridge_find_task_id` declarations
- [x] `sim-freertos-port/c/sim_kernel_bridge.c` — `sim_bridge_find_task_id()`: reverse TCB→task_id lookup for traceTASK_DELETE hook
- [x] `sim-freertos-port/c/FreeRTOSConfig.h` — `traceTASK_DELETE` macro: calls `sim_task_deleted(sim_bridge_find_task_id(pxTCB))`
- [x] `c_firmware/app/main_task_delete.c` — demo exercising vTaskDelete (other-task deletion) + xTaskCreateStatic (static allocation), 3 tasks (A creator/deleter, B deleted victim, C observer)
- [x] `crates/sim-freertos-port/build.rs` — added `main_task_delete.c` compilation
- [x] `crates/sim-runner/src/main.rs` — `--mode task-delete` CLI flag, `SimMode::TaskDelete` variant, `c_sim_task_delete_main` dispatch
- [x] `tests/traces/expected_task_delete.trace` — 19-event golden trace
- [x] `tests/golden_trace_test.sh` — `task-delete` target and `all` integration
- [x] All 195 unit tests pass; all 10 golden traces pass; `cargo fmt --check` + `cargo clippy` clean

## Quick Verification Commands

```bash
# Build
cargo build

# Run tests (195 passing)
cargo test --workspace

# Run demo (deterministic, 40-event trace)
cargo run

# Run Zephyr hello-thread demo (standalone)
cargo run -- --rtos zephyr

# Run real Zephyr (requires west workspace at ../zephyr-workspace/)
# Mode A: west build
cd ../zephyr-workspace && west build -b native_sim/native/64 zephyr/samples/hello_world
cd universal-rtos-native-simulator/
ZEPHYR_BUILD_DIR=../zephyr-workspace/build cargo run --features zephyr_real -- --rtos zephyr

# Mode B: cc crate (cross-platform — no west/SDK needed)
ZEPHYR_BASE=../zephyr-workspace/zephyr cargo run --features zephyr_real -- --rtos zephyr

# Run interactive demo (host I/O with socketpair)
cargo run -- --mode interactive

# Run broader-api demo (semaphores, mutexes, event groups, notifications)
cargo run -- --mode broader-api

# Run i2c-spi demo (I2C write/read + SPI transfer)
cargo run -- --mode i2c-spi

# Run CAN demo (CAN frames: send, receive, loopback, error state)
cargo run -- --mode can

# Run combined sensor/storage/fault-inject demo
cargo run -- --mode devices

# Run virtual entropy source (deterministic RNG) demo
cargo run -- --mode entropy

# Run task deletion + static allocation demo (vTaskDelete + xTaskCreateStatic)
cargo run -- --mode task-delete

# Run Zephyr broader-api demo (k_sem, k_mutex, k_msgq — requires real Zephyr)
ZEPHYR_BASE=../zephyr-workspace/zephyr ZEPHYR_APP=broader_api cargo run --features zephyr_real -- --rtos zephyr --mode broader-api

# Run ztest demo (Zephyr test framework — requires real Zephyr)
ZEPHYR_BASE=../zephyr-workspace/zephyr ZEPHYR_APP=ztest cargo run --features zephyr_real -- --rtos zephyr --mode ztest

# Golden trace output
cargo run -- --golden

# Format check (passing)
cargo fmt --check

# Lint check (passing)
cargo clippy --all-targets -- -D warnings

# Golden trace test (both RTOS backends)
bash tests/golden_trace_test.sh all

# Scenario golden trace tests (multi-machine simulations)
bash tests/scenario_golden_test.sh

# Run scenario from TOML file
cargo run -- run --scenario tests/scenarios/ping_pong.toml
cargo run -- run --scenario tests/scenarios/ping_pong.toml --golden

# Headless test runner (new in Phase 26)
cargo run -- test --all                    # Run all discoverable scenario tests
cargo run -- test --list                   # List discoverable scenario tests
cargo run -- test tests/scenarios/ping_pong.toml  # Run single scenario test
cargo run -- test tests/scenarios/ping_pong.toml tests/scenarios/three_chain.toml --verbose

# Interactive monitor shell (new in Phase 27)
cargo run -- shell tests/scenarios/ping_pong.toml    # Enter interactive REPL
# Shell commands (inside the REPL):
#   run, r            Run simulation to completion
#   step [n], s [n]   Advance virtual time by n ticks
#   info, i           Show full world state
#   machines, m       List machines
#   links, l          List links
#   trace, t          Show trace events
#   time              Show current virtual time
#   help, ?, h        Show help
#   quit, exit, q     Exit the shell

# CLI help
cargo run -- --help
cargo run -- test --help
cargo run -- run --help

# List available simulation modes
cargo run -- --list-modes

# JSONL trace output (machine-parseable)
cargo run -- --trace-format jsonl

# JSONL golden trace for CI comparison
cargo run -- --trace-format jsonl --golden

# Diff trace against expected file
cargo run -- --golden --diff tests/traces/expected_queue_ping_pong.trace

# Sanitizer tests (requires nightly Rust)
cargo +nightly test-asan

# Instrumented build (function-entry budget hooks)
SIM_INSTRUMENT_FUNCTIONS=1 cargo build
SIM_INSTRUMENT_FUNCTIONS=1 cargo run

# Edge-instrumented build (Tier 3 — requires Clang)
SIM_INSTRUMENT_EDGES=1 cargo build
SIM_INSTRUMENT_EDGES=1 cargo run -- --mode tight-loop

# Edge-instrumented golden trace reference (335 events vs 35 without)
SIM_INSTRUMENT_EDGES=1 cargo run -- --mode tight-loop --golden
```

## Known Limitations (per HANDOFF §19)

The competitiveness roadmap in HANDOFF.md identifies the following areas for
post-MVP development:

- **Real Zephyr integration** — `west build` support, kernel hooks, console/logging, `ztest`, CI (Phase 16: hello_world runs end-to-end via both west build and cc crate compilation)
- [x] **Broader RTOS API coverage (FreeRTOS)** — semaphores, mutexes, event groups, task notifications
- [x] **Peripheral event queue** — RTOS-agnostic `EVENT_QUEUE` with `sim_schedule_event()` C ABI; integrated into both FreeRTOS and Zephyr drain loops (Phase 18–18b)
- [x] **Scheduling architecture** — documented in `docs/scheduling.md`: RTOS kernel owns every scheduling decision, costar is the fiber substrate
- [x] **Broader RTOS API coverage (Zephyr)** — `k_sem`, `k_mutex`, `k_msgq`, `k_timer`, `k_work` (Phase 18c)
- [x] **Multi-node simulation** — `World`/`Machine`/`Link` abstractions in `crates/sim-world/`, shared virtual time, deterministic links, lockstep machine advancement, multi-machine trace collection (Phase 20)
- [x] **Scenario files** — TOML description of machines, links, packet injections, and expected traces; `--scenario` CLI flag; golden trace comparison (Phase 21)
- [x] **Platform/device ecosystem (I2C, SPI)** — `VirtualI2c` and `VirtualSpi` controllers, C ABI exports, C firmware demo, golden trace (Phase 22)
- [x] **Platform/device ecosystem (CAN)** — `VirtualCan` controller, loopback mode, error-state tracking, C ABI exports, golden trace (Phase 23)
- [x] **Platform/device ecosystem** — sensors, storage, fault injection (Phase 24)
- [x] **JSONL traces and CLI improvements** — `TraceEvent` serde::Serialize with `#[serde(tag)]` for self-describing JSONL, `--trace-format <human|jsonl>`, `--diff <path>` for trace comparison, `--list-modes`, `--verbose` (Phase 25)
- [x] **CLI/test UX** — subcommand-based CLI with `costar run`, `costar test --all`, `costar test --list`, headless CI test runner (Phase 26)
- [x] **`costar shell` interactive monitor** — REPL with run/step/info/machines/links/trace/time/help/quit commands, scenario loading (Phase 27)
- [x] **Debugging and tracing** — symbolized events (TaskCreated trace events, `--symbolicate` CLI flag, `sim_register_symbol` C ABI), GDB/LLDB support (docs/debugging.md), deterministic replay tooling (`costar replay` subcommand with `--step` mode) (Phase 28)
- [x] **Cross-platform hardening** — replaced POSIX socketpair with TCP loopback in interactive mode (works on all platforms), removed Windows-specific gating for C compilation (host poller remains Unix-only for now) (Phase 29)

### Phase 32: mcu Integration Prerequisites

These items block integrating costar as a simulation backend in the `mcu`
(mcuscaffold) Go CLI.  mcu currently uses Renode exclusively; costar would be
an additional deterministic, host-native simulation mode.

The integration model: costar runs a long-lived JSON-RPC 2.0 server (over
stdin/stdout or a Unix/TCP socket).  mcu speaks JSON-RPC to manage sessions,
load scenarios, run simulations, and retrieve traces.  This mirrors mcu's
existing Renode bridge (`mcu sim-bridge`) and keeps the language boundary
(Rust ↔ Go) at the protocol level — no CGo, no ctypes, no in-process linking.

#### 32a — JSON-RPC server (`costar serve`)

- [ ] `costar serve [--bind <addr>] [--stdio]` subcommand — starts a long-lived JSON-RPC 2.0 server
- [ ] `--stdio` mode: reads JSON-RPC requests from stdin, writes responses to stdout (one JSON object per line, newline-delimited JSON).  This is the primary mode for mcu — simple pipes, no port conflicts, no auth needed
- [ ] `--bind <addr>` mode: TCP listener (e.g. `127.0.0.1:9321`) for multi-client or remote use
- [ ] Server manages multiple concurrent simulation sessions via session IDs
- [ ] Methods:
  - `session.create` → `{session_id, state: "idle"}` — allocate a new session with its own virtual device state, event queue, and fiber registry
  - `session.destroy {session_id}` — tear down a session, free all resources
  - `session.list` → `[{session_id, state, n_machines, uptime_ticks}]` — list active sessions
  - `scenario.load {session_id, path}` → `{n_machines, n_links, n_injections}` — parse a scenario TOML into the session
  - `scenario.load_inline {session_id, toml}` → `{...}` — load a scenario from an inline TOML string (so mcu doesn't need to write temp files)
  - `sim.run {session_id}` → `{exit_code, n_events, trace_jsonl: [...], duration_ms}` — run the loaded scenario to completion
  - `sim.run_until {session_id, deadline_ticks}` → `{...}` — advance to a specific tick
  - `sim.step {session_id, n_ticks}` → `{state, now_ticks, new_events: [...]}` — advance N ticks (for interactive stepping)
  - `sim.status {session_id}` → `{state: "idle"|"running"|"done"|"error", now_ticks, n_machines}` — query simulation state
  - `sim.stop {session_id}` → `{...}` — halt a running simulation early
  - `board.configure {session_id, config_toml}` → `{n_peripherals}` — initialize virtual devices from a board peripheral config (see 32c)
  - `trace.get {session_id, format: "jsonl"|"human"}` → `{trace}` — retrieve the trace buffer
  - `server.shutdown` — graceful shutdown, completes in-flight simulations
- [ ] JSON-RPC errors use standard error codes (`-32600` parse error, `-32601` method not found, `-32602` invalid params, `-32000`+ application errors)
- [ ] `costar serve --json` prints the server's own startup metadata as JSON (`{"version": "...", "bind": "...", "pid": N}`) so mcu can parse readiness
- [ ] Unit tests: start server on random TCP port, exercise create→load→run→get-trace→destroy via raw JSON-RPC calls over TCP
- [ ] Integration test: `costar serve --stdio` with stdin-piped JSON-RPC requests, verify stdout responses

#### 32b — External Zephyr app compilation

- [x] `--zephyr-app <path>` — compile and run an external Zephyr application `.c` file instead of the hardcoded `config/app_main.c`
- [x] `--zephyr-config <dir>` — point to external config headers (autoconf.h, offsets.h, devicetree_generated.h) instead of the checked-in `config/zephyr/` defaults
- [x] `--app-sources <glob>` — additional C source files to compile alongside the main app
- [x] `--app-includes <dir>` — additional include directories
- [x] Replaces the `ZEPHYR_APP=broader_api` / `ZEPHYR_APP=ztest` env-var pattern with explicit CLI flags
- [x] `sim-zephyr-port/build.rs` refactored to accept external app paths via `DEP_ZEPHYR_APP_SOURCES` and `DEP_ZEPHYR_CONFIG_DIR` cargo directives
- [x] Golden trace for an external Zephyr app: `costar run --zephyr-app /path/to/blinky.c --zephyr-base $ZEPHYR_BASE --golden`
- [x] `scenario.load` / `scenario.load_inline` accept `app_sources`, `app_includes`, and `zephyr_config_dir` fields so mcu can send app compilation parameters over JSON-RPC without temp files

#### 32c — Board peripheral mapping (devicetree → virtual devices)

- [ ] Board config TOML that maps Zephyr devicetree labels to costar virtual device IDs, e.g.:
  ```toml
  [peripherals]
  uart0 = { device = "uart", id = 0, tx = "gpio0", rx = "gpio1" }
  i2c0 = { device = "i2c", id = 0, sda = "gpio4", scl = "gpio5" }
  spi0 = { device = "spi", id = 0, mosi = "gpio16", miso = "gpio17", sck = "gpio18" }
  gpio0 = { device = "gpio", id = 0 }
  ```
- [ ] `--board <config.toml>` CLI flag — initializes virtual devices from the board config before starting the simulator
- [ ] `board.configure` JSON-RPC method — mcu sends the board config inline, no temp files
- [ ] Board config validation: duplicate IDs, missing required port mappings, unknown device types
- [ ] Integration with mcu's generated board definitions (mcu already derives ports/pins from Zephyr devicetree via `dts2repl`; the same derivation could emit a costar board config)

#### 32d — Multi-machine UART links in scenario files

- [ ] `[[link]] type = "uart"` in scenario TOML — UART-specific link with baud rate, data bits, parity, stop bits
- [ ] `Link::Uart { baud: u32, data_bits: u8, parity: char, stop_bits: u8, latency_ticks: u64 }` variant in `sim-world/src/link.rs`
- [ ] Existing `Link::Fifo` kept for generic packet injection
- [ ] UART link delivers per-byte data at the rate implied by baud rate → virtual ticks, respecting virtual time
- [ ] Golden trace test: two machines with crossed UART links exchange data

#### 32e — mcu-side: `simmode.Costar` and JSON-RPC client

- [ ] `internal/simmode/mode.go` — add `Costar Mode = "costar"` alongside `Hardware` and `ZephyrNativeSim`
- [ ] `internal/costar/` — new Go package: JSON-RPC 2.0 client for the `costar serve` protocol
  - `costar.Start(ctx, binaryPath string) (*Client, error)` — spawns `costar serve --stdio`, connects pipes
  - `client.CreateSession()`, `client.LoadScenario()`, `client.Run()`, `client.GetTrace()`, `client.DestroySession()`, `client.Close()`
  - Handles reconnection and session lifecycle
  - Uses mcu's existing JSON-RPC patterns (the Renode bridge in `internal/bridge/` already speaks JSON-Lines over stdio — same transport, different methods)
- [ ] `internal/simulate/` — costar-aware simulation plan that generates a `costar` scenario TOML from mcu project definitions (boards → machines, connections → links, components → peripheral mappings + board configs)
- [ ] `mcu simulate --board pico --mode costar` — spawns `costar serve`, creates session, loads scenario + board config inline, runs, retrieves trace, destroys session
- [ ] `mcu build --mode costar` — uses costar's cc-crate compilation path instead of `west build`, producing a host-native binary
- [ ] E2E test: `mcu init` → `board add pico` → `component add status_led` → `simulate --mode costar` exercises the full integration pipeline via JSON-RPC

#### 32f — `costar serve` as a persistent session manager

- [ ] Sessions survive across multiple RPC calls; no need to reload scenarios or reconfigure boards between runs
- [ ] `session.clone {session_id}` → `{new_session_id}` — fork a session for A/B testing or parameter sweeps
- [ ] `sim.reset {session_id}` — reset simulation state to post-load (virtual time = 0, all machines idle, traces cleared)
- [ ] `trace.stream {session_id}` — server-sent event stream: each trace entry is pushed to mcu as it's recorded during a running simulation (real-time progress for the CLI/UI)
- [ ] Session TTL: idle sessions auto-destroy after a configurable timeout (default 5 minutes)

#### 32g — Versioning and protocol stability

- [ ] `server.version` JSON-RPC method → `{version: "1.0.0", protocol_version: 1}` — mcu calls this on connect to negotiate compatibility
- [ ] Bump costar from `0.1.0` to `1.0.0` when the JSON-RPC protocol (32a) and external app interface (32b) stabilize
- [ ] Semantic versioning from 1.0.0 forward; protocol version incremented on breaking RPC changes
- [ ] `CARGO_MSRV` documented and CI-gated

Acceptance criteria for competing with Zephyr `native_sim` and Renode-style
workflows are defined in HANDOFF.md §23.`
