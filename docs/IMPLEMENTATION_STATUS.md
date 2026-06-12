# Implementation Status

Checked items are done and verified. Unchecked items remain for future work.

## Phase 0: Repo and CI
- [x] Workspace skeleton (Cargo.toml, 7 crates)
- [x] `cargo test` passes (61 tests)
- [x] `cargo build` passes (Linux x86_64)
- [x] `cargo fmt --check` passes
- [x] `cargo clippy --all-targets -- -D warnings` passes for Rust-only crates
- [x] CI pipeline (.github/workflows/ci.yml — Linux)
- [ ] Build/test on macOS
- [ ] Build/test on Windows MSVC

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
- [ ] Sanitizer builds (leak, use-after-free)

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
- [x] `sim_hooks.c` — placeholder for future C-side trampolines
- [x] `sim_kernel_bridge.c` — TCB-to-fiber mapping array, sim_bridge_register, sim_set_current_task_by_id, pending TCB storage for deferred creation
- [x] `build.rs` — compile C port, bridge, and real FreeRTOS kernel via `cc` crate
- [x] `FreeRTOSConfig.h` — simulator configuration (cooperative, 8 priorities, static+dynamic alloc, 1ms tick, timers enabled)
- [x] pxPortInitialiseStack stores metadata on stack frame (magic, entry, param, handle slot)

## Phase 7: Real FreeRTOS Kernel (C Payload)
- [x] Real FreeRTOS-Kernel from GitHub (FreeRTOS/FreeRTOS-Kernel main branch)
- [x] `tasks.c` — full FreeRTOS task management (xTaskCreate, vTaskDelay, vTaskStartScheduler, etc.)
- [x] `queue.c` — full FreeRTOS queue implementation (xQueueCreate, xQueueSend, xQueueReceive)
- [x] `list.c` / `list.h` — real FreeRTOS list operations
- [x] `timers.c` — full FreeRTOS software timers (xTimerCreate, xTimerStart, etc.)
- [x] All required headers: `FreeRTOS.h`, `task.h`, `queue.h`, `list.h`, `timers.h`, `projdefs.h`, `portable.h`, `stack_macros.h`, `deprecated_definitions.h`, `mpu_wrappers.h`
- [x] Minimal tasks.c patches:
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
- [x] `--mode <deterministic|interactive>` CLI flag (interactive mode is a stub)
- [x] `--config <path>` CLI flag (TOML config support is a stub)
- [ ] Config file parsing (TOML deserialization)
- [ ] Interactive mode implementation (host poller, wall-clock time)

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
- [ ] Task blocks on I/O from C firmware (sim_host_block_on_fd wired into real C application)

## Phase 12: Zephyr Feasibility
- [x] Design document answering the 7 questions from HANDOFF.md §16 Phase 7 (`docs/zephyr_feasibility.md`)

## Known Limitations (per HANDOFF §19)
- [ ] No arbitrary loop preemption (cooperative fibers only)
- [ ] C UB is not sandboxed (no process isolation)
- [ ] Host-connected networking is not deterministic
- [ ] Zephyr support is future work
- [x] README with documented limitations

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

## Quick Verification Commands

```bash
# Build
cargo build

# Run tests (61 passing)
cargo test --workspace

# Run demo (40-event trace with time advancement 0→5)
cargo run

# Golden trace output
cargo run -- --golden

# Format check (passing)
cargo fmt --check

# Lint check (passing)
cargo clippy --all-targets -- -D warnings

# Golden trace test
bash tests/golden_trace_test.sh

# CLI help
cargo run -- --help
```
