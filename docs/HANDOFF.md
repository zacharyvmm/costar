# costar — Cooperative Scheduler Testing And Runtime (Implementation Handoff)

## 1. Project Description

We are building **costar** — a **Cooperative Scheduler Testing And Runtime**: a deterministic, single-threaded, cross-platform simulator engine written in **Rust 2021**. The simulator executes embedded firmware workloads natively on the host CPU without QEMU-style instruction emulation, without simulated MCU binaries, and without using host OS threads as the RTOS task abstraction.

The first target is a **FreeRTOS-compatible native simulation port** that can run mostly legacy C application code, with a smaller amount of native Rust simulation/application code. The long-term target is a reusable simulator engine that can support multiple RTOS frontends, including FreeRTOS first and Zephyr later.

The simulator’s core idea is:

> Compile embedded C/Rust firmware into a host executable, replace the RTOS hardware port layer with a Rust-controlled fiber runtime, and execute all simulated tasks under a deterministic virtual-time event loop.

This is not a hardware emulator. It is closer to a native execution harness with RTOS scheduling, virtual peripherals, virtual time, and deterministic I/O models.

## 2. High-Level Goals

### Primary Goals

1. Build a **100% Rust simulator engine**.
2. Execute mixed-language payloads:

   * approximately 90% legacy C firmware/application code,
   * approximately 10% native Rust tasks, drivers, adapters, and tests.
3. Run on:

   * Linux x86_64,
   * macOS x86_64,
   * macOS Apple Silicon,
   * Windows x86/x86_64 MSVC.
4. Avoid host-thread scheduling nondeterminism.
5. Avoid GNU linker-script dependencies in the simulator engine.
6. Avoid Python code-generation hacks in the simulator engine.
7. Provide a deterministic virtual-time event loop.
8. Support virtual RTOS tasks using stackful fibers.
9. Support non-blocking host I/O and deterministic in-memory I/O.
10. Provide a testable path toward multi-RTOS and multi-MCU simulation.

### Non-Goals for the MVP

The MVP must **not** attempt to:

1. Emulate CPU instructions.
2. Emulate an entire MCU memory map.
3. Support arbitrary Zephyr boards immediately.
4. Support preemptive native host threading.
5. Guarantee safety against arbitrary undefined behavior in C payloads.
6. Provide security isolation within the same process.
7. Reliably preempt tight infinite loops without compiler-level loop/basic-block instrumentation.
8. Support real-time wall-clock scheduling as the default mode.
9. Depend on TAP/TUN setup for deterministic tests.
10. Reuse FreeRTOS’s POSIX/pthread port as the runtime abstraction.

## 3. Architecture Summary

The simulator is split into four conceptual layers:

1. **Simulation Core**

   * Owns virtual time.
   * Owns the deterministic event queue.
   * Owns the simulation run loop.
   * Dispatches virtual interrupts, timers, peripheral events, and I/O wakeups.

2. **Fiber Runtime**

   * Owns stackful coroutine/fiber tasks.
   * Uses `corosensei` for stack switching.
   * Exposes a safe-ish Rust wrapper around task creation, resume, suspend, exit, and fault handling.
   * Maintains the currently active yielder via thread-local storage.

3. **RTOS Port Adapter**

   * Replaces the hardware-specific RTOS port layer.
   * For FreeRTOS, replaces functions/macros such as:

     * `pxPortInitialiseStack`,
     * `xPortStartScheduler`,
     * `vPortYield` / `portYIELD`,
     * critical-section hooks,
     * tick interrupt hooks.
   * Maps RTOS tasks to Rust-managed fibers.

4. **Virtual Device and I/O Layer**

   * Provides deterministic virtual devices.
   * Provides optional host-connected adapters.
   * Uses non-blocking I/O only.
   * Integrates `polling` or `mio` for host event notification.
   * Uses `smoltcp` for deterministic in-process TCP/IP where appropriate.

## 4. Design Principles

### 4.1 Determinism First

The simulator must produce the same event trace for the same firmware, configuration, seed, and input script.

This means:

* no dependence on host wall-clock time in deterministic mode,
* no dependence on host thread scheduling,
* no unordered iteration in externally visible behavior,
* no random values unless seeded through the simulator,
* no host I/O timing inside deterministic replay tests,
* no `HashMap` iteration order in driver init, event logs, or scheduling decisions.

When a collection order matters, use:

* `BTreeMap`,
* sorted `Vec`,
* explicit sequence numbers,
* `IndexMap` only if insertion order is intentionally part of the model.

### 4.2 One Host Thread

The simulator core must run on one host thread. Do not use:

* `std::thread::spawn`,
* pthreads,
* Windows native threads,
* Tokio multithreaded runtime,
* async executors that hide worker threads.

A single-threaded async poller is acceptable for host I/O adapters, but it must be integrated into the simulator event loop, not run independently.

### 4.3 Virtual Time, Not Wall Time

The simulator has a monotonic virtual clock:

```rust
pub type Tick = u64;
```

The unit should be configurable, but the MVP should use nanoseconds or microseconds internally. All virtual timers, sleeps, network delays, and peripheral events must be scheduled against `Tick`.

Wall-clock time is allowed only in an optional “interactive” mode, never in deterministic tests.

### 4.4 Rust Engine, C Payload

The simulator engine must be Rust-first and Rust-owned. C is treated as guest firmware/application payload and RTOS kernel code.

Rust owns:

* event queue,
* task/fiber registry,
* virtual time,
* device models,
* network models,
* tracing,
* deterministic replay,
* test harness,
* host I/O adapters.

C owns:

* legacy firmware logic,
* FreeRTOS kernel code,
* application tasks,
* selected C drivers being tested.

### 4.5 Bounded Unsafe

Unsafe Rust is expected but must be localized.

Allowed unsafe zones:

* C FFI exports/imports,
* raw yielder pointer in TLS,
* coroutine stack management,
* conversion of C task entry pointers,
* optional raw socket registration,
* panic/fault boundary handling.

Everything else should be safe Rust.

Every unsafe block must include a short safety comment explaining:

* who owns the pointer,
* how lifetime is bounded,
* why aliasing is acceptable,
* why the call cannot unwind across C,
* what assumptions the C side must satisfy.

## 5. Recommended Workspace Layout

```text
costar/
  Cargo.toml
  build.rs
  crates/
    sim-core/
      src/
        lib.rs
        time.rs
        event_queue.rs
        run_loop.rs
        trace.rs
        config.rs

    sim-fiber/
      src/
        lib.rs
        task.rs
        stack.rs
        tls.rs
        yield_reason.rs
        panic_boundary.rs

    sim-ffi/
      src/
        lib.rs
        freertos_exports.rs
        c_types.rs
      include/
        sim_abi.h
        sim_portmacro.h

    sim-devices/
      src/
        lib.rs
        uart.rs
        gpio.rs
        timer.rs
        irq.rs

    sim-net/
      src/
        lib.rs
        smoltcp_device.rs
        host_poll.rs
        packet_script.rs

    sim-freertos-port/
      src/
        lib.rs
      c/
        port.c
        portmacro.h
        sim_hooks.c

    sim-runner/
      src/
        main.rs

  c_firmware/
    app/
    freertos/
      include/
      tasks.c
      queue.c
      timers.c
      list.c

  tests/
    c/
      yield_loop.c
      queue_ping_pong.c
      sleep_task.c
    traces/
      expected_blinky.trace
      expected_queue_ping_pong.trace

  docs/
    architecture.md
    ffi_contract.md
    determinism.md
    free_rtos_port_notes.md
    zephyr_feasibility.md
```

## 6. Core Event Queue

### 6.1 Required Behavior

The event queue must be a strict deterministic min-heap ordered by:

1. absolute virtual timestamp,
2. priority,
3. insertion sequence number.

Do not place a callback directly inside the ordered key type. A callback is not comparable and should not participate in ordering.

Recommended structure:

```rust
use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub type Tick = u64;
pub type EventId = u64;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub struct QueueKey {
    pub at: Tick,
    pub priority: u16,
    pub seq: u64,
    pub id: EventId,
}

pub type EventCallback = Box<dyn FnOnce(&mut SimulatorContext) + 'static>;

pub struct ScheduledEvent {
    pub key: QueueKey,
    pub callback: Option<EventCallback>,
    pub label: &'static str,
}

pub struct EventQueue {
    heap: BinaryHeap<Reverse<QueueKey>>,
    events: std::collections::BTreeMap<EventId, ScheduledEvent>,
    next_id: EventId,
    next_seq: u64,
}
```

Lower `priority` values should run first. For example:

* priority `0`: fatal simulator control events,
* priority `10`: virtual IRQs,
* priority `20`: RTOS tick,
* priority `30`: device completion,
* priority `40`: host I/O wakeup,
* priority `100`: background maintenance.

### 6.2 Event Queue API

Implement:

```rust
impl EventQueue {
    pub fn schedule_at(
        &mut self,
        at: Tick,
        priority: u16,
        label: &'static str,
        callback: EventCallback,
    ) -> EventId;

    pub fn schedule_after(
        &mut self,
        now: Tick,
        delta: Tick,
        priority: u16,
        label: &'static str,
        callback: EventCallback,
    ) -> EventId;

    pub fn cancel(&mut self, id: EventId) -> bool;

    pub fn pop_next(&mut self) -> Option<ScheduledEvent>;

    pub fn peek_time(&self) -> Option<Tick>;

    pub fn is_empty(&self) -> bool;
}
```

Cancellation should remove the event from the `events` map and leave a tombstoned heap entry. When a tombstoned key is popped, skip it.

### 6.3 Simulator Run Loop

The core run loop must:

1. pop the next event,
2. advance virtual time to the event timestamp,
3. execute the event callback,
4. drain guest RTOS execution until all runnable guest tasks yield, block, sleep, or exit,
5. process host I/O only if no virtual event is due earlier,
6. stop when:

   * no events remain,
   * a stop condition is met,
   * a fatal simulator error occurs,
   * a test deadline is reached.

Skeleton:

```rust
pub struct SimulatorCore {
    pub now: Tick,
    pub queue: EventQueue,
    pub running: bool,
    pub trace: TraceSink,
}

impl SimulatorCore {
    pub fn run(&mut self, ctx: &mut SimulatorContext) -> SimResult<()> {
        while self.running {
            let Some(event) = self.queue.pop_next() else {
                break;
            };

            if event.key.at < self.now {
                return Err(SimError::TimeWentBackwards {
                    now: self.now,
                    event_at: event.key.at,
                });
            }

            self.now = event.key.at;
            self.trace.event_dispatch(self.now, event.label);

            if let Some(callback) = event.callback {
                callback(ctx);
            }

            ctx.drain_rtos_scheduler(self.now)?;
        }

        Ok(())
    }
}
```

## 7. Fiber Runtime

### 7.1 Fiber Model

Every simulated RTOS task maps to one stackful coroutine.

Each fiber stores:

* task ID,
* task name,
* configured RTOS stack size,
* actual host stack size,
* task state,
* coroutine handle,
* RTOS metadata pointer if applicable,
* pending unblock reason,
* last yield reason,
* creation sequence number.

Minimum host stack size must be enforced. The initial default should be:

```rust
const MIN_HOST_COROUTINE_STACK: usize = 64 * 1024;
```

Embedded RTOS tasks may request very small stacks, but host C libraries and debug logging can easily exceed embedded stack assumptions. The simulator should preserve the requested stack size as metadata but allocate at least the simulator minimum for the host coroutine.

### 7.2 Yield Reasons

Define explicit yield reasons:

```rust
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum YieldReason {
    Cooperative,
    RtosPortYield,
    Blocked,
    SleepUntil(Tick),
    IoWait,
    InterruptExit,
    TaskExit,
    BudgetExceeded,
    Fault,
}
```

Define resume reasons:

```rust
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ResumeReason {
    Start,
    SchedulerSelected,
    TimeoutExpired,
    IoReady,
    InterruptReturn,
    Manual,
}
```

The fiber runtime should use typed input/output values, not bare `()` everywhere. This makes traces and debugging significantly easier.

### 7.3 Thread-Local Active Yielder

C port hooks need a way to suspend the currently active coroutine. Use TLS to store a pointer to the active yielder only during a coroutine resume window.

Recommended pattern:

```rust
use std::cell::Cell;
use std::ptr::NonNull;
use corosensei::Yielder;

pub type SimYielder = Yielder<ResumeReason, YieldReason>;

thread_local! {
    static ACTIVE_YIELDER: Cell<Option<NonNull<SimYielder>>> =
        const { Cell::new(None) };
}
```

Rules:

1. Set `ACTIVE_YIELDER` immediately before entering/resuming a coroutine.
2. Clear it immediately when returning to the parent stack.
3. Never store the pointer beyond the active resume window.
4. Never call `suspend` if no yielder is active.
5. Never let a Rust panic cross the C ABI.
6. Assert single-threaded execution in debug builds.

The exported C function should not expose `corosensei` directly:

```rust
#[no_mangle]
pub unsafe extern "C" fn sim_port_yield() {
    ACTIVE_YIELDER.with(|cell| {
        if let Some(ptr) = cell.get() {
            let yielder = ptr.as_ref();
            yielder.suspend(YieldReason::RtosPortYield);
        } else {
            // In production, record an error instead of panicking across FFI.
            sim_record_fatal_error(SimErrorCode::YieldWithoutActiveFiber);
        }
    });
}
```

Do not name this function `vPortYield` in Rust unless the C port layer requires that exact symbol. Prefer exporting simulator-owned ABI names and mapping RTOS macros to them in `portmacro.h`.

## 8. FreeRTOS Port Adapter

### 8.1 MVP Scope

The MVP should support a minimal FreeRTOS subset:

* task creation,
* task start,
* cooperative yield,
* virtual tick,
* `vTaskDelay`,
* queues,
* software timers if feasible,
* critical sections,
* task exit/delete later.

Start with two C tasks:

1. task A prints/increments a counter and delays,
2. task B receives from a queue and delays.

Do not begin with networking.

### 8.2 Port Layer Strategy

Do not use the upstream FreeRTOS POSIX port as the runtime model because it relies on host threading. Instead, create a custom simulator port.

Files:

```text
sim-freertos-port/c/port.c
sim-freertos-port/c/portmacro.h
sim-freertos-port/c/sim_hooks.c
```

The port must define or override:

```c
StackType_t *pxPortInitialiseStack(
    StackType_t *pxTopOfStack,
    TaskFunction_t pxCode,
    void *pvParameters
);

BaseType_t xPortStartScheduler(void);
void vPortEndScheduler(void);

void vPortYield(void);
void vPortEnterCritical(void);
void vPortExitCritical(void);
void vPortSuppressTicksAndSleep(TickType_t xExpectedIdleTime);
```

The C port layer should delegate task/fiber lifecycle to Rust through `sim_abi.h`.

### 8.3 C ABI Header

Create a stable handwritten C ABI. Avoid bindgen for the MVP.

Example:

```c
#ifndef SIM_ABI_H
#define SIM_ABI_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef void (*sim_task_entry_fn)(void *);

typedef enum sim_yield_reason {
    SIM_YIELD_COOPERATIVE = 0,
    SIM_YIELD_RTOS_PORT = 1,
    SIM_YIELD_BLOCKED = 2,
    SIM_YIELD_SLEEP = 3,
    SIM_YIELD_IO = 4,
    SIM_YIELD_TASK_EXIT = 5,
} sim_yield_reason_t;

typedef uintptr_t sim_task_handle_t;

uint64_t sim_now_ticks(void);

sim_task_handle_t sim_create_task(
    const char *name,
    sim_task_entry_fn entry,
    void *arg,
    uint32_t requested_stack_words,
    uint32_t priority
);

void sim_start_scheduler(void);
void sim_port_yield(void);
void sim_task_exit(void);

void sim_enter_critical(void);
void sim_exit_critical(void);

void sim_trace_u32(const char *label, uint32_t value);

#ifdef __cplusplus
}
#endif

#endif
```

### 8.4 Task Creation Mapping

`pxPortInitialiseStack` is normally responsible for creating an initial CPU register frame on an embedded target. In the simulator, it should instead register enough metadata for Rust to create a coroutine.

The implementation agent must decide whether to:

1. create the Rust fiber directly during `pxPortInitialiseStack`, or
2. store a small fake context in the provided FreeRTOS stack and create the Rust fiber lazily when the scheduler starts.

The safer MVP path is lazy creation:

* `pxPortInitialiseStack` stores:

  * task entry pointer,
  * task parameter pointer,
  * magic value,
  * optional simulator task handle.
* `xPortStartScheduler` walks/receives task metadata and starts the Rust scheduler.

If direct creation is easier, ensure FreeRTOS still receives a plausible `StackType_t *` and stack overflow checks do not break.

### 8.5 Scheduler Ownership

The simulator must not reimplement FreeRTOS scheduling policy unless absolutely necessary.

FreeRTOS should continue to own:

* task priorities,
* ready lists,
* delay lists,
* queues,
* timers,
* task state transitions.

Rust should own:

* the actual host coroutine for each task,
* when a coroutine is resumed,
* when virtual time advances,
* virtual interrupt/tick delivery,
* host I/O wakeups.

The Rust scheduler drain loop should repeatedly resume whichever task the FreeRTOS port layer says is currently selected, until all tasks are blocked, sleeping, or no progress can be made.

### 8.6 Tick Handling

For MVP, use periodic ticks, not tickless idle.

1. Schedule an RTOS tick event every `tick_period`.
2. On each tick event:

   * call FreeRTOS tick increment hook,
   * request a context switch if needed,
   * schedule next tick,
   * drain scheduler.

Later, optimize to tickless mode by querying the next unblock time.

### 8.7 Critical Sections

Critical sections should not use host CPU interrupt masking or OS locks.

Represent virtual interrupt state inside the simulator:

```rust
pub struct InterruptState {
    pub nesting: u32,
    pub locked: bool,
}
```

C hooks:

```c
void vPortEnterCritical(void) {
    sim_enter_critical();
}

void vPortExitCritical(void) {
    sim_exit_critical();
}
```

Rust behavior:

* increment nesting on enter,
* decrement nesting on exit,
* set `locked = nesting > 0`,
* defer virtual interrupt delivery while locked,
* deliver pending virtual interrupts after unlock.

## 9. Native Rust Task Support

Rust tasks should use the same fiber runtime as C tasks.

Provide an API like:

```rust
pub fn spawn_rust_task<F>(
    &mut self,
    name: &'static str,
    priority: u32,
    stack_size: usize,
    f: F,
) -> TaskId
where
    F: FnOnce(TaskContext) + 'static;
```

Rust task context:

```rust
pub struct TaskContext {
    pub task_id: TaskId,
}

impl TaskContext {
    pub fn yield_now(&self);
    pub fn sleep_until(&self, at: Tick);
    pub fn sleep_for(&self, delta: Tick);
    pub fn now(&self) -> Tick;
}
```

Do not use Rust `async` as the primary task abstraction in the MVP. Stackful fibers are required because the C payload expects blocking call stacks.

## 10. Networking and Host I/O

### 10.1 Split Deterministic and Host-Connected Modes

Networking must have two modes:

1. **Deterministic mode**

   * no host sockets,
   * no wall-clock I/O,
   * packet inputs come from scripted traces,
   * packet outputs are captured and compared,
   * all wakeups are scheduled by virtual time.

2. **Host-connected mode**

   * uses non-blocking sockets,
   * uses `polling` or `mio`,
   * can talk to localhost or external services,
   * not guaranteed bit-for-bit deterministic,
   * useful for demos and integration testing.

This split is mandatory. Do not let host I/O timing leak into deterministic tests.

### 10.2 `smoltcp` Integration

Use `smoltcp` as the in-process TCP/IP stack for deterministic network tests.

Implement a simulator network device:

```rust
pub struct SimNetDevice {
    rx_queue: VecDeque<Packet>,
    tx_queue: VecDeque<Packet>,
    mtu: usize,
}
```

The smoltcp device should:

* consume packets from `rx_queue`,
* write outgoing packets to `tx_queue`,
* use simulator virtual time for timestamps,
* schedule virtual IRQ/wakeup events when packets arrive,
* support packet capture for golden trace tests.

### 10.3 Host Poller Integration

For host-connected mode:

* all sockets must be non-blocking,
* register file descriptors/sockets with `polling::Poller` or `mio::Poll`,
* never call blocking `recv`, `accept`, `connect`, or `read` from inside a fiber,
* if a C task calls `recv()` and no data is available:

  1. register interest with the host poller,
  2. mark the simulated task blocked on I/O,
  3. call `sim_port_yield`,
  4. resume another simulated task or advance virtual time.

Host poller wait duration must be bounded by the next virtual event deadline. In deterministic tests, the poller must not be used.

## 11. CPU-Bound Stall Mitigation

### 11.1 Reality Check

Stackful fibers are cooperative. If a C task does this:

```c
while (1) {
}
```

the entire simulator will hang unless the binary has been instrumented in a way that inserts checks inside the loop or basic blocks.

Function-entry instrumentation alone is not enough. It can catch deep call chains and recursive behavior, but it will not repeatedly fire inside a tight infinite loop that never calls another function.

### 11.2 MVP Policy

For MVP:

1. Document that C code must eventually call an RTOS blocking primitive, yield, delay, queue receive, semaphore wait, or instrumented hook.
2. Add test-level wall-clock watchdogs to detect simulator hangs.
3. Add optional function-entry instrumentation for budget checks.
4. Do not claim full preemption until loop/basic-block instrumentation exists.

### 11.3 Instrumentation Options

Support these tiers:

#### Tier 0: No Instrumentation

Works for well-behaved RTOS code that yields, delays, or blocks.

#### Tier 1: Function Entry/Exit Instrumentation

For GCC/Clang:

```c
void __cyg_profile_func_enter(void *this_fn, void *call_site);
void __cyg_profile_func_exit(void *this_fn, void *call_site);
```

Use this to:

* count function calls,
* check task budget on entry,
* force a cooperative yield if budget expired,
* trace hot functions.

Do not use this as a complete infinite-loop solution.

#### Tier 2: Manual Loop Hooks

Provide a macro for firmware under test:

```c
#define SIM_LOOP_POLL() sim_budget_poll(__FILE__, __LINE__)
```

Developers can add:

```c
while (1) {
    SIM_LOOP_POLL();
    // work
}
```

#### Tier 3: Compiler Basic-Block or Loop-Edge Instrumentation

Future work:

* LLVM pass,
* Clang sanitizer coverage hooks,
* source-to-source transformation,
* build-time macro injection for selected files.

This tier is required for robust infinite-loop control.

### 11.4 Budget API

Rust should expose:

```rust
pub struct TaskBudget {
    pub max_function_entries: u64,
    pub max_virtual_ticks_without_yield: u64,
    pub exceeded: bool,
}
```

C hook:

```c
void sim_budget_poll(const char *file, int line);
```

Rust behavior:

* if budget is exceeded, record trace entry,
* mark current task as budget-yielded,
* suspend the fiber,
* allow scheduler/event loop to run.

## 12. Compile-Time Registration

### 12.1 Goal

Replace project-maintained GNU linker-script registries with Rust-side registration.

This is especially relevant for simulated drivers:

* UART models,
* GPIO models,
* timer models,
* network devices,
* virtual sensors,
* interrupt controllers.

### 12.2 `inventory` Registry

Example:

```rust
pub struct SimulatedDriver {
    pub name: &'static str,
    pub init_order: u32,
    pub init_func: unsafe extern "C" fn() -> i32,
}

inventory::collect!(SimulatedDriver);

inventory::submit! {
    SimulatedDriver {
        name: "uart0",
        init_order: 100,
        init_func: init_uart0,
    }
}
```

Important: registry iteration order must not be trusted. Always collect, sort, then initialize:

```rust
let mut drivers: Vec<&'static SimulatedDriver> =
    inventory::iter::<SimulatedDriver>.into_iter().collect();

drivers.sort_by_key(|d| (d.init_order, d.name));

for driver in drivers {
    unsafe {
        (driver.init_func)();
    }
}
```

### 12.3 C Driver Registration

For C drivers, prefer explicit Rust-side adapter registration.

Example:

```rust
extern "C" {
    fn c_uart0_init() -> i32;
}

inventory::submit! {
    SimulatedDriver {
        name: "c_uart0",
        init_order: 100,
        init_func: c_uart0_init,
    }
}
```

Do not rely on C custom sections in the MVP.

## 13. Build Pipeline

### 13.1 `build.rs`

Use the `cc` crate for FreeRTOS MVP C payload compilation.

Example:

```rust
fn main() {
    println!("cargo:rerun-if-changed=c_firmware/app/main.c");
    println!("cargo:rerun-if-changed=c_firmware/freertos/tasks.c");
    println!("cargo:rerun-if-changed=crates/sim-freertos-port/c/port.c");

    let mut build = cc::Build::new();

    build
        .file("c_firmware/app/main.c")
        .file("c_firmware/freertos/tasks.c")
        .file("c_firmware/freertos/queue.c")
        .file("c_firmware/freertos/list.c")
        .file("c_firmware/freertos/timers.c")
        .file("crates/sim-freertos-port/c/port.c")
        .file("crates/sim-freertos-port/c/sim_hooks.c")
        .include("c_firmware/freertos/include")
        .include("crates/sim-ffi/include")
        .define("SIMULATION_HOST_MODE", Some("1"))
        .define("FREERTOS_PORT_SIM", Some("1"));

    if cfg!(any(target_os = "linux", target_os = "macos")) {
        build.flag_if_supported("-Wall");
        build.flag_if_supported("-Wextra");
        build.flag_if_supported("-fno-omit-frame-pointer");
        build.flag_if_supported("-finstrument-functions");
    }

    if cfg!(target_env = "msvc") {
        build.flag_if_supported("/W3");
        // Do not blindly enable /Gh or /GH until x64 hook ABI is validated.
    }

    build.compile("embedded_c_payload");
}
```

### 13.2 MSVC Instrumentation Warning

Do not assume GCC/Clang hooks exist on MSVC.

For Windows/MSVC:

* keep instrumentation disabled by default,
* implement cooperative RTOS yield first,
* later evaluate `/Gh` and `/GH`,
* validate x86 versus x64 behavior separately,
* do not rely on `__declspec(naked)` for x64 hook definitions.

### 13.3 Zephyr Build Warning

Supporting unmodified Zephyr is not the same as compiling a few C files through `cc`.

Zephyr uses its own build/configuration system, including CMake, Kconfig, and devicetree-generated headers. For this project, treat Zephyr as a later adapter with its own feasibility phase.

For MVP:

* FreeRTOS is in scope.
* Zephyr is a design target, not an MVP target.
* “No Python/codegen hacks” applies to this simulator engine, not necessarily to upstream Zephyr’s own build pipeline.

## 14. Public Rust API Sketch

```rust
pub struct Simulator {
    core: SimulatorCore,
    ctx: SimulatorContext,
}

impl Simulator {
    pub fn new(config: SimConfig) -> Self;

    pub fn spawn_rust_task<F>(
        &mut self,
        name: &'static str,
        priority: u32,
        stack_size: usize,
        f: F,
    ) -> TaskId
    where
        F: FnOnce(TaskContext) + 'static;

    pub fn schedule_at<F>(
        &mut self,
        at: Tick,
        priority: u16,
        label: &'static str,
        f: F,
    ) -> EventId
    where
        F: FnOnce(&mut SimulatorContext) + 'static;

    pub fn run(&mut self) -> SimResult<()>;

    pub fn run_until(&mut self, deadline: Tick) -> SimResult<()>;

    pub fn run_until_idle(&mut self) -> SimResult<()>;

    pub fn trace(&self) -> &TraceSink;
}
```

## 15. Trace and Replay

Every deterministic run should be able to emit a trace.

Trace entries:

```rust
pub enum TraceEvent {
    EventScheduled {
        at: Tick,
        priority: u16,
        label: &'static str,
    },
    EventDispatched {
        at: Tick,
        label: &'static str,
    },
    TaskResume {
        at: Tick,
        task: TaskId,
        reason: ResumeReason,
    },
    TaskYield {
        at: Tick,
        task: TaskId,
        reason: YieldReason,
    },
    InterruptRaised {
        at: Tick,
        irq: u32,
    },
    InterruptDelivered {
        at: Tick,
        irq: u32,
    },
    PacketRx {
        at: Tick,
        len: usize,
    },
    PacketTx {
        at: Tick,
        len: usize,
    },
    Fatal {
        at: Tick,
        code: SimErrorCode,
    },
}
```

Golden trace tests must compare deterministic output to expected traces.

## 16. Testing Plan

### Phase 0: Repo and CI

Acceptance criteria:

* `cargo test` runs on Linux, macOS, and Windows.
* `cargo fmt --check` passes.
* `cargo clippy --all-targets -- -D warnings` passes for Rust-only crates.
* C build works on all target OSes for a tiny C file.

### Phase 1: Event Queue

Tests:

1. same timestamp, different priority,
2. same timestamp and priority, insertion order,
3. cancellation,
4. tombstone skipping,
5. no time rollback,
6. deterministic trace output.

Acceptance criteria:

* 100,000 randomly scheduled events pop in expected order.
* Same seed produces same trace on all host OSes.

### Phase 2: Fiber Runtime

Tests:

1. create one fiber,
2. yield/resume 1,000,000 times,
3. create many fibers,
4. task exit,
5. panic boundary,
6. minimum stack enforcement,
7. TLS active yielder guard.

Acceptance criteria:

* no leaks in sanitizer-supported builds,
* no use-after-free when a task exits,
* no active yielder remains after resume returns,
* no panic crosses C ABI.

### Phase 3: C FFI Yield Harness

Create a C function:

```c
void c_task(void *arg) {
    for (int i = 0; i < 10; i++) {
        sim_trace_u32("i", i);
        sim_port_yield();
    }
}
```

Acceptance criteria:

* Rust starts C task in a fiber.
* C task yields to Rust 10 times.
* Trace order is deterministic.
* Windows/macOS/Linux all pass.

### Phase 4: FreeRTOS Minimal Port

Tests:

1. two tasks yield back and forth,
2. `vTaskDelay`,
3. queue send/receive,
4. critical section nesting,
5. tick dispatch,
6. task priority ordering.

Acceptance criteria:

* no host threads,
* deterministic task interleaving,
* periodic virtual tick works,
* basic FreeRTOS application completes or reaches expected idle state.

### Phase 5: Virtual Devices

Implement:

* virtual UART,
* virtual timer,
* basic interrupt controller.

Tests:

* UART write trace,
* timer interrupt wakes task,
* interrupt deferred during critical section,
* interrupt delivered after critical exit.

### Phase 6: Networking

First deterministic mode:

* in-memory packet injection,
* smoltcp interface,
* packet capture trace.

Then host-connected mode:

* non-blocking localhost socket,
* poller wakeup,
* task blocks instead of freezing,
* no blocking syscalls in fiber path.

### Phase 7: Zephyr Feasibility

Deliver a design document answering:

1. Can we support Zephyr by adding a custom architecture/board?
2. Which Zephyr build outputs must be consumed?
3. How much of native_sim can be reused conceptually?
4. Which parts rely on Linux/POSIX assumptions?
5. Can Zephyr thread switching be mapped to `corosensei`?
6. What generated headers/config artifacts are unavoidable?
7. What is the smallest Zephyr “hello thread” proof of concept?

Zephyr implementation should not start until this feasibility document is complete.

## 17. Risk Register

### Risk 1: C Undefined Behavior Can Corrupt the Process

Rust does not sandbox unsafe C memory writes.

Mitigation:

* run sanitizers in CI where available,
* keep C ABI narrow,
* add guard pages to coroutine stacks where supported,
* consider process isolation later for untrusted firmware.

### Risk 2: Infinite Loops Freeze the Simulator

Cooperative fibers cannot preempt tight loops.

Mitigation:

* require cooperative RTOS blocking in MVP,
* add watchdog tests,
* add function-entry budget hooks,
* add loop/basic-block instrumentation later.

### Risk 3: FreeRTOS Internals Are Port-Sensitive

FreeRTOS ports are tightly coupled to scheduler internals.

Mitigation:

* start with minimal kernel subset,
* keep simulator port small,
* avoid modifying core FreeRTOS files where possible,
* isolate port assumptions in `sim-freertos-port`.

### Risk 4: Host I/O Breaks Determinism

Host sockets depend on host timing.

Mitigation:

* deterministic packet scripts for tests,
* host-connected mode clearly marked non-deterministic,
* record/replay packet captures.

### Risk 5: `inventory` Registration Order Is Not Stable

Mitigation:

* explicit `init_order`,
* sort by `(init_order, name)`,
* never depend on registry iteration order.

### Risk 6: Windows MSVC Instrumentation Differs From GCC/Clang

Mitigation:

* disable instrumentation by default on MSVC,
* support cooperative yield first,
* add Windows hook backend only after ABI validation.

### Risk 7: Zephyr Is Not FreeRTOS

Zephyr has a more complex build/configuration model.

Mitigation:

* FreeRTOS MVP first,
* Zephyr feasibility phase,
* avoid promising Python-free unmodified Zephyr builds,
* treat Zephyr as an adapter, not as a drop-in compile-through-`cc` target.

## 18. Implementation Order

Implement in this exact order:

1. Workspace skeleton.
2. `sim-core` event queue.
3. Deterministic trace sink.
4. `sim-fiber` single fiber proof.
5. 1,000,000 yield/resume stress test.
6. TLS active yielder guard.
7. C ABI header and Rust exports.
8. Tiny C task yielding into Rust.
9. FreeRTOS compile through `cc`.
10. FreeRTOS simulator `port.c` / `portmacro.h`.
11. Two-task FreeRTOS yield test.
12. Virtual tick event.
13. `vTaskDelay` test.
14. Queue send/receive test.
15. Critical-section simulation.
16. Virtual UART.
17. Deterministic packet-device skeleton.
18. `smoltcp` deterministic loopback.
19. Optional host poller adapter.
20. Zephyr feasibility document.

## 19. Definition of Done for MVP

The MVP is complete when:

1. The simulator builds on Linux, macOS, and Windows MSVC.
2. The Rust simulator engine uses no host threads.
3. A C FreeRTOS application with at least two tasks runs inside Rust-managed fibers.
4. The tasks can yield, delay, and communicate through a FreeRTOS queue.
5. Virtual time advances deterministically.
6. The event queue has stable ordering.
7. The same test produces the same trace on all supported host OSes.
8. A blocked C task does not block the host process.
9. Unsafe code is isolated and documented.
10. The README clearly documents limitations:

    * no arbitrary loop preemption yet,
    * C UB is not sandboxed,
    * host-connected networking is not deterministic,
    * Zephyr support is future work.

## 20. Agent Instructions

When implementing, prefer correctness over cleverness.

Do not:

* introduce host threads,
* introduce Tokio,
* use async/await as the RTOS task model,
* depend on GNU linker scripts,
* depend on Python codegen for the simulator engine,
* assume Linux-only behavior,
* use nondeterministic iteration order,
* let panics cross C ABI,
* hide unsafe pointer lifetime assumptions.

Do:

* write small tests per phase,
* commit working vertical slices,
* keep FFI narrow,
* make traces human-readable,
* use explicit virtual time everywhere,
* keep FreeRTOS-specific code out of the generic simulator core,
* sort all registries before initialization,
* document every platform-specific compromise.

The first successful demo should be:

> A host executable, built with Cargo, that compiles a tiny FreeRTOS C app and runs two simulated FreeRTOS tasks on Rust-managed stackful fibers. The tasks exchange a queue message and use `vTaskDelay`; the Rust event loop advances virtual time and emits an identical trace across Linux, macOS, and Windows.

## 21. Competitiveness Roadmap

### 21.1 Goal

Make `costar` a serious competitor to:

1. **Zephyr `native_sim`** for cross-platform host-native Zephyr and RTOS testing.
2. **Renode-style workflows** for deterministic multi-node embedded system simulation.

The recommended positioning is:

> `costar` is a deterministic, cross-platform, native RTOS simulation framework with Renode-style multi-node testing goals.

Do **not** market it as a Renode replacement yet unless it eventually supports unmodified target binaries or an emulator backend.

## 22. Missing Areas

### 22.1 Real Zephyr Integration

The Zephyr work is now operational beyond proof-of-concept: real kernel compilation
via cc crate, west build support, multi-fiber thread switching, timer driver,
console/logging, `ztest` framework, and broader API coverage (k_sem, k_mutex,
k_msgq, k_timer, k_work).  Remaining gaps: networking, filesystem, and Bluetooth
subsystem support (see §§25-28); Windows MASM stubs for `linker_stubs.S`; and CI
automation for 5+ real Zephyr sample tests.

### 22.2 Subsystem Coverage (Networking / BT / Filesystem)

**Current state**: The Rust-side virtual device models (`VirtualEthDevice`,
`FlatMemoryStore`, `VirtualHciController`) are complete with 34 unit tests
(7 eth + 9 block + 9 bt + 5 smoltcp + 4 tcp).  The C ABI exports (17
`#[no_mangle]` functions) and RTOS-agnostic C stub drivers (5 files across
both RTOS ports) are in place.  FreeRTOS golden trace demos exercise all
three devices end-to-end:
  * `--mode net` — Ethernet loopback (37 events, `tests/traces/expected_net.trace`)
  * `--mode block` — Block device write/read/erase (28 events, `tests/traces/expected_block.trace`)
  * `--mode bt` — HCI command/event/ACL exchange (29 events, `tests/traces/expected_bt.trace`)

**Deterministic networking (smoltcp)**: `SmoltcpBridge`
(`crates/sim-net/src/smoltcp_bridge.rs`) routes VirtualEthDevice frames
through the smoltcp TCP/IP stack (ARP, ICMP, TCP, UDP).  Configured as
10.0.0.1/24 as the simulator-side peer.  The scheduler's
`eth_loopback_bridge()` uses the smoltcp bridge when available and falls
back to simple loopback otherwise.  5 unit tests cover ARP reply generation,
ICMP echo reply, no-op idle, and unconsumed-frame forwarding.

**Host-connected networking (TCP bridge)**: `TcpBridge`
(`crates/sim-net/src/tcp_bridge.rs`) connects VirtualEthDevice to a
remote TCP endpoint via non-blocking I/O with a 2-byte big-endian length
frame protocol.  4 unit tests cover connect/send/receive, multi-frame,
disconnect detection, and partial-read reassembly.

**Scripted BLE event injection**: The scenario DSL supports
`[[inject]] type = "ble_event"` entries in TOML files.  The World dispatch
loop injects HCI events into `VirtualHciController` at specified virtual
times.  Supported event types: `connection_complete`, `acl_data`,
`disconnect`, `advertising_report`.  HCI controllers are auto-registered.
Golden trace test scenario at `tests/scenarios/ble_inject.toml` (2 events).

**What remains**: The actual RTOS networking/filesystem/BT stacks (Zephyr's
LwIP, littlefs, BT host; FreeRTOS+TCP, FreeRTOS+FAT) are not yet compiled
into the simulator.  The virtual devices are functional but not wired to
any guest RTOS IP/filesystem stack.  See §§25-28 for per-subsystem designs
and remaining effort (~37 days for RTOS stack integration).

### 22.3 Multi-Node Simulation

[DONE] World/Machine/Link abstractions, shared virtual time, deterministic
links (FIFO + UART), CAN bus topology, scenario files (TOML), headless CI
test runner, interactive monitor shell.

### 22.4 Platform/Device Modeling

[DONE — extensive] Virtual device ecosystem: UART, GPIO, timer, I2C, SPI,
CAN, ADC, temperature sensor, EEPROM, Flash, fault injector, deterministic
entropy source.  `inventory`-based compile-time driver registration.

Remaining gaps: virtual Ethernet device (see §25), virtual block device for
filesystems (see §27), virtual HCI controller for Bluetooth (see §26).

### 22.5 CLI/Test UX

[DONE] Subcommand-based CLI (`costar run`, `costar test --all`, `costar shell`,
`costar replay`), JSONL + human trace formats, golden trace comparison,
`--diff`, `--machine-filter`, `--symbolicate`, `--board`, `--zephyr-app`.

### 22.6 Debugging and Tracing

[DONE] JSONL traces, task/device/machine inspection, symbolized events,
GDB/LLDB support (docs/debugging.md), deterministic replay tooling.

### 22.7 Cross-Platform Hardening

[DONE for core] Linux, macOS, Windows MSVC all pass CI.  Interactive mode
uses TCP loopback (cross-platform).  Host poller remains Unix-only.
Zephyr Windows build requires MASM stubs for `linker_stubs.S`.

### 22.8 JSON-RPC Server and mcu Integration

[DONE for costar side] `costar serve` JSON-RPC 2.0 server with full session
management, 14 RPC methods, stdio + TCP transport, session TTL, streaming
traces, protocol version negotiation.  Go reference client in `mcu/`.

Remaining (mcu repo side): `simmode.Costar`, simulation plan generation,
`mcu simulate --mode costar` end-to-end pipeline.

## 23. Acceptance Criteria

### 23.1 Native Sim Competitor

costar can credibly claim to compete with Zephyr `native_sim` when:

* A real Zephyr app builds through `west`. [DONE]
* The app runs through costar on Linux, macOS, and Windows. [PARTIAL — Windows needs MASM stubs]
* Basic Zephyr kernel primitives work. [DONE]
* Console/logging works. [DONE]
* `ztest` pass/fail behavior works. [DONE]
* At least 5 real Zephyr samples/tests pass in CI. [TODO]
* Deterministic traces are stable across repeated runs. [DONE]
* Limitations are documented honestly. [DONE]
* Zephyr networking stack (LwIP) runs through costar's virtual Ethernet driver. [TODO — see §25]
* Zephyr filesystem (littlefs or FAT) mounts on costar's virtual block device. [TODO — see §27]

### 23.2 Renode-Style Competitor

costar can credibly claim to be Renode-style when:

* It supports multiple machines in one simulation. [DONE]
* Machines share deterministic virtual time. [DONE]
* Machines communicate through virtual links. [DONE — FIFO, UART, CAN bus]
* Scenario files describe machines, devices, links, inputs, and expectations. [DONE]
* There is a headless test runner for CI. [DONE]
* There is an interactive monitor/debug shell. [DONE]
* Traces include machine/device/task-level events. [DONE]
* At least one realistic multi-node demo exists. [DONE — microcar 4-machine CAN + plant simulation]
* Documentation explains that costar runs host-native RTOS payloads, not unmodified MCU binaries. [DONE]
* Multi-machine networking (Ethernet links between machines) is functional. [TODO — extends §25]
* Multi-machine Bluetooth (BLE connections between machines) is functional. [TODO — extends §26]

### 23.3 Full RTOS Subsystem Support

costar can credibly claim full RTOS subsystem support when:

* A Zephyr or FreeRTOS firmware opens a TCP socket, connects to a peer, sends
  and receives data, and closes the socket — all within a deterministic
  simulation, producing the same trace every run.
* A Zephyr firmware mounts a littlefs filesystem, creates a file, writes
  data, reads it back, and unlinks the file — all within a deterministic
  simulation.
* A Zephyr firmware advertises as a BLE peripheral, a simulated peer
  connects, exchanges GATT reads/writes, and disconnects — all within a
  deterministic simulation, driven by scripted HCI events.
* All three subsystem tests produce golden traces that pass CI on Linux,
  macOS, and (where applicable) Windows.

## 24. Strategic Recommendation

The shortest path to credibility is:

1. Stabilize the FreeRTOS MVP.
2. Make Linux/macOS/Windows CI real.
3. Run real Zephyr `west` builds.
4. Add `ztest`, console, logging, and kernel primitive support.
5. Add multi-machine world scheduling.
6. Add scenario files and a headless test runner.

This gives costar a unique identity:

> Faster and more portable than Zephyr `native_sim`, lighter and more native-code-focused than Renode, with deterministic multi-node RTOS testing as the long-term differentiator.

## 25. Networking Subsystem Design

### 25.1 Current State

The Rust-side `VirtualEthDevice` (`crates/sim-net/src/eth_device.rs`, 7 unit tests)
and C ABI exports (`sim_eth_register`, `sim_eth_send`, `sim_eth_recv`,
`sim_eth_poll`, `sim_eth_on_recv`) are complete.  RTOS-agnostic C stub drivers
(`sim-freertos-port/c/sim_eth.c`, `sim-zephyr-port/c/sim_eth.c`) are compiled
via cc crate.  The scheduler's `eth_loopback_bridge()` enables deterministic
frame delivery.  A FreeRTOS demo (`--mode net`) exchanges
Ethernet frames between two tasks (37-event golden trace).

**What is done**:
- `VirtualEthDevice` Rust model (FIFO queues, MAC, MTU, rx callback)
- 5 C ABI exports + C stub drivers for both RTOS ports
- FreeRTOS loopback demo with golden trace
- `SmoltcpBridge` (`crates/sim-net/src/smoltcp_bridge.rs`, 5 unit tests): deterministic
  smoltcp TCP/IP stack integration.  VirtualEthDevice frames route through
  `SimNetDevice` → smoltcp `Interface` (10.0.0.1/24) → back to guest.
  Supports ARP, ICMP echo, TCP, UDP.  Scheduler falls back to loopback
  when no bridge configured.
- `TcpBridge` (`crates/sim-net/src/tcp_bridge.rs`, 4 unit tests): host-connected
  mode via non-blocking TCP socket with length-framed Ethernet protocol.
  Supports disconnect detection and partial-read reassembly.
- `TapBridge` (`crates/sim-net/src/tap_bridge.rs`, 6 unit tests): host-connected
  mode via host TAP interface (Linux `/dev/net/tun`, macOS `/dev/tapN` with
  tuntaposx kernel extension).  Raw Ethernet frames are read/written on the TAP
  file descriptor.  Non-blocking I/O with `HostPoller` integration for scheduler
  wakeups.  The `--tap <ifname>` CLI flag bridges guest VirtualEthDevice frames
  to/from the host network stack.

**What remains**:
- Guest firmware (Zephyr or FreeRTOS+TCP) cannot use its own networking stack.
  A Zephyr app calling `socket()`/`bind()`/`send()` hits a `CONFIG_NETWORKING=n`
  build-time dead end.
- Zephyr networking Kconfig/cc-crate compilation (LwIP subsystem)
- FreeRTOS+TCP compilation via cc crate

### 25.2 Design Principle

Replace the hardware-specific network driver, not the networking stack itself.
Zephyr's LwIP (or native IP stack) and FreeRTOS+TCP run **unmodified**.  We
provide a virtual Ethernet driver that talks to costar's `SimNetDevice` (or to
host sockets in interactive mode), following the same pattern as the existing
RTOS port layers:

```
┌────────────────────────────────────────────────────────────┐
│  Guest firmware: socket() / send() / recv() / connect()    │
├────────────────────────────────────────────────────────────┤
│  RTOS IP stack (unmodified)                                │
│  Zephyr: LwIP or native IP stack → net_if → net_context   │
│  FreeRTOS: FreeRTOS+TCP → FreeRTOS_IPInit → NetworkBuffer  │
├────────────────────────────────────────────────────────────┤
│  NEW: Virtual Ethernet driver (costar)                     │
│  · Replaces eth_native_posix.c (Zephyr)                    │
│  · Replaces NetworkInterface_t (FreeRTOS+TCP)              │
│  · Routes frames to/from SimNetDevice or HostPoller        │
├────────────────────────────────────────────────────────────┤
│  costar sim-net                                            │
│  · SimNetDevice — deterministic smoltcp, rx/tx queues      │
│  · HostPoller — host sockets (interactive, non-det.)       │
│  · World links — inter-machine FIFO/UART links             │
└────────────────────────────────────────────────────────────┘
```

### 25.3 Zephyr Networking

Zephyr's networking stack is configurable via Kconfig.  The minimum path
enables the following:

```
CONFIG_NETWORKING=y
CONFIG_NET_L2_ETHERNET=y
CONFIG_NET_IPV4=y
CONFIG_NET_IPV6=y
CONFIG_NET_TCP=y
CONFIG_NET_UDP=y
CONFIG_NET_SOCKETS=y
CONFIG_NET_SOCKETS_SOCKOPT_TLS=n    # skip TLS for MVP
```

The Ethernet driver interface that must be implemented:

| Zephyr API | costar Implementation |
|-----------|----------------------|
| `eth_iface_init(net_if)` | Register the interface, set MTU, attach to `SimNetDevice` |
| `eth_send(net_if, pkt)` | Push `pkt` → `SimNetDevice::inject_rx` (smoltcp rx queue) |
| Driver-level `recv` callback | Pull frames from `SimNetDevice::drain_tx` → push to Zephyr's `net_if` via `net_pkt` allocation |
| `eth_start()` / `eth_stop()` | Link up/down state flag |

The virtual Ethernet driver lives in `crates/sim-zephyr-port/c/sim_eth.c`.
It replaces Zephyr's `eth_native_posix.c`.  Frame flow:

```
Deterministic mode (smoltcp):
  Zephyr net_if → sim_eth_send() → SimNetDevice inject_rx
  SimNetDevice drain_tx → sim_eth_recv_into_net_if() → Zephyr net_if

Interactive mode (host sockets):
  Zephyr net_if → sim_eth_send() → HostPoller send() on host socket
  HostPoller recv() → sim_eth_recv_into_net_if() → Zephyr net_if
```

The `net_if` interface is polled periodically from the simulator's drain loop.
When `SimNetDevice` has pending rx frames, the virtual Ethernet driver
registers a `sim_schedule_event()` callback that calls `net_if_recv_data()`.

### 25.4 FreeRTOS+TCP Networking

FreeRTOS+TCP provides its own IP stack.  The hardware interface is
`NetworkInterface_t` (a struct of function pointers):

| FreeRTOS+TCP API | costar Implementation |
|-----------------|----------------------|
| `pxNetworkInterface->pxOutputFunction(pkt, len)` | Push frame → `SimNetDevice::inject_rx` |
| `pxNetworkInterface->pxGetPhyLinkStatus()` | Return `pdTRUE` (link always up) |
| `xNetworkInterfaceInitialise()` | Attach to `SimNetDevice`, set MAC address |
| Incoming frame delivery | `eConsiderFrameForProcessing(data, len)` called from drain hook |

The driver lives in `crates/sim-freertos-port/c/sim_eth.c`.  It registers a
`NetworkInterface_t` with FreeRTOS+TCP's `FreeRTOS_IPInit_Multi()`.  Incoming
frames are delivered via a `sim_schedule_event()` callback that calls
`eConsiderFrameForProcessing()`.

### 25.5 Deterministic vs. Host-Connected Modes

Following the existing pattern (§10.1), networking has two modes:

1. **Deterministic mode** (default): Both Zephyr and FreeRTOS+TCP stacks route
   through `SimNetDevice` (smoltcp).  All traffic is scripted — packet injection
   via scenario files.  Full golden-trace compatibility.

2. **Host-connected mode** (`--mode interactive`): A TAP interface (Linux/macOS)
   or a TCP bridge (Windows) connects the virtual Ethernet device to the host
   network.  Not deterministic; useful for demos and integration testing.

The dual-mode switch is compile-time (feature gate `host-net`) plus runtime
(`--mode interactive`).  Deterministic tests gate on `cfg(not(feature =
"host-net"))` or the runtime mode check.

### 25.6 Wi-Fi and 802.15.4

Wi-Fi (802.11) and 802.15.4 (Thread/Zigbee) are deferred to a later phase.
The architecture supports them through the same `net_if` driver pattern —
a virtual Wi-Fi driver would present as `CONFIG_NET_L2_WIFI` with a
costar-backed L2 layer.  The MAC layer (association, scanning, encryption)
requires significantly more simulation infrastructure and is out of scope
for the networking MVP.

### 25.7 New C ABI Exports

```
// Register a virtual Ethernet device with the simulator.
uint32_t sim_eth_register(uint32_t id, const uint8_t *mac, uint32_t mtu);

// Send an Ethernet frame from the guest. Returns bytes queued.
uint32_t sim_eth_send(uint32_t id, const uint8_t *data, uint32_t len);

// Receive the next Ethernet frame into buf. Returns bytes written.
uint32_t sim_eth_recv(uint32_t id, uint8_t *buf, uint32_t buf_size);

// Check if any rx frames are pending for this Ethernet device.
uint32_t sim_eth_poll(uint32_t id);

// Register a receive callback (called when frames arrive).
void sim_eth_on_recv(uint32_t id, void (*callback)(void));
```

### 25.8 New Rust Module

`crates/sim-net/src/eth_device.rs`:

```rust
pub struct VirtualEthDevice {
    id: u32,
    mac: [u8; 6],
    mtu: usize,
    rx_queue: VecDeque<Vec<u8>>,
    rx_callback: Option<unsafe extern "C" fn()>,
}

impl VirtualEthDevice {
    pub fn new(id: u32, mac: [u8; 6], mtu: usize) -> Self;
    pub fn send(&mut self, data: &[u8]) -> usize;     // guest → rx queue
    pub fn recv_into(&mut self, buf: &mut [u8]) -> usize; // tx queue → guest
    pub fn inject_rx(&mut self, frame: Vec<u8>);       // host/test → guest
    pub fn drain_tx(&mut self) -> Vec<Vec<u8>>;         // collect guest output
    pub fn on_recv(&mut self, cb: unsafe extern "C" fn());
}
```

### 25.9 Build Integration

For Zephyr cc-crate compilation, enable the networking config options by
adding pre-generated `autoconf.h` entries:

```c
#define CONFIG_NETWORKING 1
#define CONFIG_NET_L2_ETHERNET 1
#define CONFIG_NET_IPV4 1
#define CONFIG_NET_TCP 1
#define CONFIG_NET_UDP 1
#define CONFIG_NET_SOCKETS 1
#define CONFIG_NET_SOCKETS_POSIX_NAMES 1
```

The Ethernet driver (`sim_eth.c`) is compiled alongside the existing arch layer.
Zephyr's networking subsystems (`subsys/net/`) are compiled from `ZEPHYR_BASE`
via the existing cc-crate path, replacing `eth_native_posix.c` with `sim_eth.c`.

For FreeRTOS, FreeRTOS+TCP is compiled via the cc crate alongside the existing
FreeRTOS kernel files, with the custom `NetworkInterface` replacing the
hardware-specific one.

### 25.10 Estimated Effort

| Task | Effort | Risk | Status |
|------|--------|------|--------|
| VirtualEthDevice (Rust) | 3 days | Low — mirrors SimNetDevice pattern | ✓ |
| C ABI exports (sim_eth_*) | 1 day | Low — standard FFI pattern | ✓ |
| sim_eth.c (Zephyr Ethernet driver) | 5 days | Medium — must match Zephyr's net_if API contract | ✓ |
| sim_eth.c (FreeRTOS+TCP driver) | 3 days | Medium — FreeRTOS+TCP's NetworkInterface is simpler | ✓ |
| Zephyr networking config (Kconfig fragments, autoconf.h) | 2 days | Medium — Kconfig dependency resolution | ✓ |
| smoltcp integration (deterministic mode) | 3 days | Low — existing SimNetDevice is smoltcp-ready | ✓ |
| TAP/host-connected mode | 3 days | Medium — platform-specific TAP setup | ✓ |
| Golden trace tests (TCP echo, UDP round-trip) | 4 days | Low |
| FreeRTOS+TCP integration (cc crate build) | 2 days | Low |
| **Total networking MVP** | **~26 days** | |

---

## 26. Bluetooth Subsystem Design

### 26.1 Current State

The Rust-side `VirtualHciController` (`crates/sim-devices/src/bt.rs`, 9 unit tests)
and C ABI exports (`sim_bt_register`, `sim_bt_send`, `sim_bt_recv`,
`sim_bt_inject_event`, `sim_bt_on_recv`) are complete.  A Zephyr HCI driver stub
(`sim-zephyr-port/c/sim_hci.c`) is compiled via cc crate.  A FreeRTOS demo
(`--mode bt`) exchanges HCI commands, events, and ACL data between two tasks
(29-event golden trace).

**What is done**:
- `VirtualHciController` Rust model (cmd/event/ACL FIFOs, advertising state, scripted responses)
- 5 C ABI exports + Zephyr HCI driver stub
- `HciPacket`/`HciPacketType`/`HciCommand` Rust types
- FreeRTOS HCI demo with golden trace
- Scripted BLE event injection via scenario DSL (`[[inject]] type = "ble_event"`):
  scenario TOML `tests/scenarios/ble_inject.toml`, World dispatch with
  `BleInjection` scheduling, HCI controller auto-registration, golden trace
  (2 events).  Supports `connection_complete`, `acl_data`, `disconnect`,
  `advertising_report` event types.

**What remains**:
- Zephyr BT host is not yet compiled into the simulator (no `CONFIG_BT` Kconfig)
- Golden trace test against real Zephyr BT host (advertising → connection → GATT exchange)

### 26.2 Design Principle

Zephyr's Bluetooth subsystem is split into a **host** (upper layers: GATT,
L2CAP, SMP, etc.) and a **controller** (lower layer: HCI, link layer, radio).
The host communicates with the controller via the Host Controller Interface
(HCI) — a standardized protocol over UART, SPI, or USB.

We replace the HCI transport driver, not the BT stack itself.  Zephyr's BT
host runs unmodified.  We provide a **virtual HCI controller** that routes
packets through costar's event system.

```
┌────────────────────────────────────────────────────────────┐
│  Guest firmware: bt_conn_create_le(), bt_gatt_write(), ... │
├────────────────────────────────────────────────────────────┤
│  Zephyr BT Host (unmodified)                               │
│  subsys/bluetooth/host/ — GATT, L2CAP, ATT, SMP, conn, ... │
│  HCI Core: bt_recv(), bt_send() → hci_core.c              │
├────────────────────────────────────────────────────────────┤
│  NEW: Virtual HCI driver (costar)                          │
│  · Replaces hci_uart.c or hci_spi.c                       │
│  · Routes HCI packets to/from VirtualHciController         │
├────────────────────────────────────────────────────────────┤
│  costar sim-bt                                             │
│  · VirtualHciController — HCI command/event/data FIFOs     │
│  · Scripted BLE events for deterministic testing           │
│  · Host-bridge mode for interactive (future)               │
└────────────────────────────────────────────────────────────┘
```

### 26.3 HCI Contract

HCI uses four packet types:

| Type | Direction | Purpose |
|------|-----------|---------|
| Command | Host → Controller | Configure controller, start scan, connect, etc. |
| Event | Controller → Host | Command completion, connection events, disconnection |
| ACL Data | Bidirectional | L2CAP data (GATT, SMP, etc.) |
| ISO Data | Bidirectional | LE Audio (future) |

Each packet has a 1-byte type header followed by a type-specific payload.
The virtual controller processes commands and generates events — it does not
need to simulate an actual radio.

### 26.4 Virtual HCI Controller

`crates/sim-devices/src/bt.rs` (new):

```rust
pub struct VirtualHciController {
    id: u32,
    /// HCI commands received from the host.
    cmd_queue: VecDeque<HciPacket>,
    /// HCI events to deliver to the host.
    event_queue: VecDeque<HciPacket>,
    /// ACL data from host (to be delivered to peer).
    acl_host_tx: VecDeque<HciPacket>,
    /// ACL data for host (from peer).
    acl_host_rx: VecDeque<HciPacket>,
    /// Advertising state.
    advertising: bool,
    /// Connected peer address (if any).
    connected_peer: Option<[u8; 6]>,
    /// Scripted responses: (command_opcode, response_event_data).
    script: BTreeMap<u16, Vec<u8>>,
    /// Receive callback registered by the HCI driver.
    rx_callback: Option<unsafe extern "C" fn()>,
}

pub enum HciPacketType {
    Command = 1,
    AclData = 2,
    ScoData = 3,
    Event = 4,
    IsoData = 5,
}
```

The controller handles a minimal subset of HCI commands for the MVP:

| HCI Command | Response |
|-------------|----------|
| `HCI_Reset` | `CommandComplete(HCI_Reset, Status=0)` |
| `LE_Set_Advertising_Data` | `CommandComplete` with success |
| `LE_Set_Advertising_Parameters` | `CommandComplete` with success |
| `LE_Set_Advertising_Enable` | `CommandComplete`; sets `advertising` flag |
| `LE_Create_Connection` | `CommandStatus(Pending)` → after N ticks: `LE_Connection_Complete` with simulated peer |
| `Disconnect` | `CommandStatus(Pending)` → `Disconnection_Complete` |
| `LE_Read_Local_Supported_Features` | `CommandComplete` with feature mask |

### 26.5 Scripted BLE Events

For deterministic testing, scenario files inject BLE events at specific
virtual times:

```toml
[[inject]]
at_ms = 250
type = "ble_event"
controller = 0
event = "connection_complete"
params = { peer_addr = "AA:BB:CC:DD:EE:FF", interval_ms = 30 }

[[inject]]
at_ms = 500
type = "ble_event"
controller = 0
event = "acl_data"
params = { handle = 0, data = "48656c6c6f" }  # "Hello" hex
```

The virtual controller queues these as HCI Event or ACL Data packets,
delivered to the host on the next `sim_schedule_event()` tick.

### 26.6 FreeRTOS Bluetooth

FreeRTOS does not include a native Bluetooth stack.  For FreeRTOS-based
firmware, Bluetooth support would come through a third-party host stack
(e.g., NimBLE ported to FreeRTOS) communicating via the same virtual HCI
controller.  The HCI transport is RTOS-agnostic — the same `VirtualHciController`
serves both Zephyr and FreeRTOS.

### 26.7 New C ABI Exports

```c
// Register a virtual HCI controller.
uint32_t sim_bt_register(uint32_t id);

// Send an HCI command or ACL data packet from the host.
void sim_bt_send(uint32_t id, uint8_t packet_type,
                 const uint8_t *data, uint32_t len);

// Receive the next HCI event or ACL data packet for the host.
// Returns bytes written, or 0 if empty.
uint32_t sim_bt_recv(uint32_t id, uint8_t *packet_type,
                     uint8_t *buf, uint32_t buf_size);

// Inject a scripted HCI event into the controller.
void sim_bt_inject_event(uint32_t id, const uint8_t *data, uint32_t len);

// Register a receive callback (called when events arrive for the host).
void sim_bt_on_recv(uint32_t id, void (*callback)(void));
```

### 26.8 Build Integration

For Zephyr, enable the BT subsystem:

```c
#define CONFIG_BT 1
#define CONFIG_BT_HCI 1
#define CONFIG_BT_HCI_HOST 1           // host-only, controller is external (us)
#define CONFIG_BT_CONN 1
#define CONFIG_BT_MAX_CONN 4
#define CONFIG_BT_MAX_PAIRED 4
#define CONFIG_BT_GATT_CLIENT 1
#define CONFIG_BT_GATT_SERVER 1
#define CONFIG_BT_L2CAP_TX_BUF_COUNT 4
```

The virtual HCI driver replaces Zephyr's `hci_uart.c` or `hci_spi.c`.
The driver registers with Zephyr's HCI core via `bt_hci_driver_register()`.

### 26.9 Estimated Effort

| Task | Effort | Risk |
|------|--------|------|
| VirtualHciController (Rust) | 4 days | Medium — HCI spec is substantial, but MVP subset is small |
| C ABI exports (sim_bt_*) | 1 day | Low |
| sim_hci.c (Zephyr HCI driver) | 3 days | Low — 200-line driver, well-defined API |
| BT config (Kconfig fragments, autoconf.h) | 1 day | Low |
| Scripted BLE event injection | 2 days | Low — extends existing scenario DSL |
| Golden trace tests (GATT read, advertising) | 3 days | Medium — need realistic BT traces |
| **Total BT MVP** | **~14 days** | |

---

## 27. Filesystem Subsystem Design

### 27.1 Current State

The Rust-side `FlatMemoryStore` (`crates/sim-devices/src/block.rs`, 9 unit tests)
and C ABI exports (`sim_block_create`, `sim_block_read`, `sim_block_write`,
`sim_block_erase_page`, `sim_block_get_geometry`, `sim_block_snapshot`,
`sim_block_restore`) are complete.  RTOS-agnostic C stub drivers
(`sim-freertos-port/c/sim_block.c`, `sim-zephyr-port/c/sim_flash.c`) are
compiled via cc crate.  A FreeRTOS demo (`--mode block`) exercises write,
read, erase, and geometry queries between two tasks (28-event golden trace).

**What is done**:
- `FlatMemoryStore` Rust model (page-addressed, read/write/erase, write/erase counts, snapshot/restore)
- 7 C ABI exports + C stub drivers for both RTOS ports
- FreeRTOS block device demo with golden trace

**What remains**:
- Guest firmware cannot mount a real filesystem (no `open()`/`read()`/`write()`)
- Zephyr FS Kconfig/cc-crate compilation (`CONFIG_FLASH`, `CONFIG_FILE_SYSTEM_LITTLEFS`)
- FreeRTOS+FAT compilation via cc crate
- Snapshot/restore from host filesystem for deterministic replay

### 27.2 Design Principle

Replace the storage media driver, not the filesystem itself.  Zephyr's
littlefs/FAT and FreeRTOS+FAT run **unmodified**.  We provide a virtual
block device backend that maps read/write/erase operations to costar's
in-memory storage models.

```
┌────────────────────────────────────────────────────────────┐
│  Guest firmware: open(), read(), write(), close(), lseek()  │
├────────────────────────────────────────────────────────────┤
│  RTOS Filesystem (unmodified)                               │
│  Zephyr: littlefs or FAT → fs/fs_ops → file system API     │
│  FreeRTOS: FreeRTOS+FAT → ff_fopen/ff_fread/ff_fwrite      │
├────────────────────────────────────────────────────────────┤
│  NEW: Virtual block device (costar)                         │
│  · Replaces flash driver (Zephyr) or media driver (FR+)    │
│  · Maps block read/write/erase to FlatMemoryStore          │
├────────────────────────────────────────────────────────────┤
│  costar sim-storage                                         │
│  · FlatMemoryStore — page-addressed, deterministic         │
│  · Optional: snapshot/save to host filesystem               │
│  · VirtualEeprom / VirtualFlash (existing, reused)          │
└────────────────────────────────────────────────────────────┘
```

### 27.3 RTOS-Agnostic Block Device

The virtual block device is an RTOS-agnostic Rust model — it does not depend
on Zephyr's or FreeRTOS's FS internals.  Both RTOSes use the same
`FlatMemoryStore`:

`crates/sim-devices/src/block.rs` (new):

```rust
/// A deterministic, page-addressed virtual block device.
///
/// This is the backend for both Zephyr's littlefs/FAT and FreeRTOS+FAT.
/// The guest filesystem issues read/write/erase operations at page
/// granularity; FlatMemoryStore records them and can be snapshotted
/// for deterministic replay.
pub struct FlatMemoryStore {
    id: u32,
    /// Page size in bytes (typically 256, 512, 2048, or 4096).
    page_size: u32,
    /// Total number of pages.
    page_count: u32,
    /// Page data (flat allocation, size = page_size * page_count).
    pages: Vec<u8>,
    /// Write count per page (for wear-leveling simulation).
    write_counts: Vec<u64>,
    /// Erase count per page.
    erase_counts: Vec<u64>,
    /// Erased byte value (0xFF for flash, 0x00 for EEPROM).
    erase_value: u8,
}

impl FlatMemoryStore {
    /// Create a new block device with all pages erased.
    pub fn new(id: u32, page_size: u32, page_count: u32, erase_value: u8) -> Self;

    /// Read `len` bytes from an absolute offset into `buf`.
    /// Returns the number of bytes actually read.
    pub fn read(&self, offset: u32, buf: &mut [u8]) -> u32;

    /// Write `len` bytes to an absolute offset.
    /// Returns the number of bytes actually written.
    /// Before writing, the target page must be erased (all erased_value).
    pub fn write(&mut self, offset: u32, data: &[u8]) -> u32;

    /// Erase the page containing the given absolute offset.
    /// Sets all bytes in that page to `erase_value`.
    pub fn erase_page(&mut self, offset: u32);

    /// Save the current state to a host file (for snapshot-based determinism).
    pub fn snapshot(&self, path: &str) -> io::Result<()>;

    /// Restore state from a host file.
    pub fn restore(path: &str) -> io::Result<Self>;
}
```

### 27.4 Zephyr Filesystem Integration

Zephyr's filesystem stack uses the `disk` driver interface (for FAT) or
the `flash` driver interface (for littlefs).  The virtual driver implements
the flash driver API:

| Zephyr Flash API | costar Implementation |
|-----------------|----------------------|
| `flash_read(dev, offset, data, len)` | `FlatMemoryStore::read(offset, buf)` |
| `flash_write(dev, offset, data, len)` | `FlatMemoryStore::write(offset, data)` |
| `flash_erase(dev, offset, size)` | `FlatMemoryStore::erase_page(offset)` per page |
| `flash_get_page_info_by_offs(dev, offset, ...) ` | Returns page size/count from `FlatMemoryStore` |

The driver is compiled alongside Zephyr's flash subsystem.  The Kconfig:

```c
#define CONFIG_FLASH 1
#define CONFIG_FLASH_PAGE_LAYOUT 1
#define CONFIG_FLASH_SIMULATOR 1
#define CONFIG_FILE_SYSTEM 1
#define CONFIG_FILE_SYSTEM_LITTLEFS 1
#define CONFIG_FS_LOG_LEVEL_OFF 1
```

### 27.5 FreeRTOS+FAT Integration

FreeRTOS+FAT uses a media driver interface (`FF_Disk_t`).  The virtual driver
implements the required functions:

| FreeRTOS+FAT API | costar Implementation |
|-----------------|----------------------|
| `FF_Read(pxDisk, ulSector, pvBuffer, ulCount)` | `FlatMemoryStore::read(ulSector * sector_size, buf)` |
| `FF_Write(pxDisk, ulSector, pvBuffer, ulCount)` | `FlatMemoryStore::write(ulSector * sector_size, data)` |
| `FF_GetCapacity(pxDisk)` → sector count | Returns `page_count * page_size / sector_size` |
| `FF_GetStatus(pxDisk)` | Returns `pdTRUE` (media always present) |
| `FF_Init(pxDisk)` | Initialize `FlatMemoryStore`, attach to disk handle |

### 27.6 Deterministic Snapshots

For golden-trace determinism, `FlatMemoryStore` can be snapshotted to a host
file at the start of a simulation and restored for replay.  The trace captures
all write/erase operations.  This allows:

1. A test that writes files → verifies trace output.
2. A test that reads previously written files → restores snapshot, verifies
   read data matches.
3. Multi-machine scenarios where each machine has its own filesystem image.

### 27.7 New C ABI Exports

```c
// Create a new virtual block device.
uint32_t sim_block_create(uint32_t id, uint32_t page_size,
                          uint32_t page_count, uint8_t erase_value);

// Read from the block device at an absolute offset.
uint32_t sim_block_read(uint32_t id, uint32_t offset,
                        uint8_t *buf, uint32_t len);

// Write to the block device at an absolute offset.
uint32_t sim_block_write(uint32_t id, uint32_t offset,
                         const uint8_t *data, uint32_t len);

// Erase the page containing an absolute offset.
void sim_block_erase_page(uint32_t id, uint32_t offset);

// Get geometry of the block device.
void sim_block_get_geometry(uint32_t id, uint32_t *page_size,
                            uint32_t *page_count);

// Snapshot the block device to a host file.
int32_t sim_block_snapshot(uint32_t id, const char *path);

// Restore a block device from a host file.
int32_t sim_block_restore(uint32_t id, const char *path);
```

### 27.8 Estimated Effort

| Task | Effort | Risk |
|------|--------|------|
| FlatMemoryStore (Rust) | 3 days | Low — pure data model, no scheduling |
| C ABI exports (sim_block_*) | 1 day | Low |
| sim_flash.c (Zephyr flash driver) | 3 days | Low — well-defined flash API |
| sim_block.c (FreeRTOS+FAT media driver) | 2 days | Low — ~150 lines |
| FS config (Kconfig fragments, autoconf.h) | 2 days | Low |
| Snapshot/restore | 2 days | Low — serde to/from file |
| Golden trace tests (file create/write/read/delete) | 3 days | Low |
| **Total filesystem MVP** | **~16 days** | |

---

## 28. Complete Subsystem Roadmap Summary

| Subsystem | Effort | Current Status | Priority |
|-----------|--------|---------------|----------|
| Networking (Zephyr + FreeRTOS+TCP) | ~26 days (foundation + smoltcp + TCP + TAP done; ~11d remaining) | Foundation + smoltcp + TCP/TAP bridge + FreeRTOS demo done | **High** |
| Filesystem (littlefs/FAT) | ~16 days (foundation done; ~10d remaining) | Foundation + FreeRTOS demo done | **Medium** |
| Bluetooth (Zephyr BT host + virtual HCI) | ~14 days (foundation + BLE DSL done; ~7d remaining) | Foundation + BLE scenario DSL + FreeRTOS demo done | **Low** |

**Simulator engine complete**: Rust models (40 unit tests), C ABI exports (17 functions),
C stub drivers (5 files), FreeRTOS golden trace demos (94 events across 3 modes).
**SmoltcpBridge** (ARP/ICMP/TCP/UDP, 5 tests), **TcpBridge** (host-connected TCP,
4 tests), and **TapBridge** (host-connected TAP, 6 tests) provide deterministic,
TCP-bridged, and TAP-bridged networking.  **Scripted BLE event
injection** via scenario DSL with World dispatch, auto-controller-registration,
and golden trace.  314 unit tests + 14 golden traces + 4 scenario traces pass.
All golden traces pass CI on macOS (Apple Silicon).  `cargo fmt --check` +
`cargo clippy --all-targets -- -D warnings` clean.

**Remaining effort: ~28 days** for RTOS stack integration:
  * Networking: Zephyr LwIP Kconfig + cc-crate compilation, FreeRTOS+TCP build
    (~11 days — smoltcp, TCP bridge, and TAP bridge done)
  * Filesystem: Zephyr littlefs Kconfig + cc-crate compilation, FreeRTOS+FAT build (~10 days)
  * Bluetooth: Zephyr BT host Kconfig + cc-crate compilation (~7 days — BLE DSL done)

### 28.1 Implementation Order

1. **Filesystem first** — simplest, no scheduling complexity, pure data model.
   Builds on existing `VirtualEeprom`/`VirtualFlash`.  Validates the RTOS-agnostic
   virtual device pattern for higher-level subsystems.
   [DONE — Rust model + C ABI + C driver + FreeRTOS golden trace demo]

2. **Networking second** — most impactful for real-world firmware testing.
   Leverages existing `smoltcp` integration.  The Ethernet driver pattern is
   well-precedented in both Zephyr and FreeRTOS+TCP.
   [DONE — Rust model + C ABI + C driver + FreeRTOS golden trace demo]

3. **Bluetooth last** — most complex (HCI state machine), niche relative to
   the other two.  Focus on Zephyr BT host since FreeRTOS has no native BT stack.
   [DONE — Rust model + C ABI + C driver + FreeRTOS golden trace demo]

4. **Zephyr RTOS stack integration** — compile real LwIP, littlefs, and BT host
   via cc crate with Kconfig/autoconf.h fragments.  Wire the existing virtual
   device backends into each stack's driver interface.  ~37 days remaining.
   [TODO]

### 28.2 Integration Pattern (All Three Subsystems)

Every subsystem follows the same integration pattern:

1. **New Rust module** in `sim-devices` or `sim-net` — the deterministic model
   [DONE for all three: `eth_device.rs`, `block.rs`, `bt.rs`, `smoltcp_bridge.rs`, `tcp_bridge.rs`, `tap_bridge.rs` — 40 unit tests]
2. **New C ABI exports** in `sim-ffi` — `#[no_mangle]` functions exposed to C
   [DONE for all three: 17 functions in `sim_abi.h` + `lib.rs`]
3. **New C driver** in the RTOS port crate — replaces the hardware-specific driver
   [DONE for all three: `sim_eth.c` x2, `sim_flash.c`, `sim_block.c`, `sim_hci.c`]
4. **Config additions** — Kconfig fragments (Zephyr) or `#define` blocks (FreeRTOS)
   [TODO — requires real RTOS stack compilation]
5. **Scenario DSL extensions** — scripted input injection for deterministic tests
   [DONE — BLE event injection via `[[inject]] type = "ble_event"` with World dispatch and golden trace]
6. **Golden trace tests** — verified output within a deterministic simulation
   [DONE for all three — `--mode net`, `--mode block`, `--mode bt`]

Steps 1-3, 5, and 6 are complete.  Remaining work (step 4) requires compiling
the actual RTOS stacks (LwIP, littlefs, BT host) via the cc crate.  This is
the next major phase — estimated ~31 days.

