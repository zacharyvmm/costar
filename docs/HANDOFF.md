# Universal RTOS Native Simulator — Implementation Handoff

## 1. Project Description

We are building a **Universal RTOS Native Simulator**: a deterministic, single-threaded, cross-platform simulator engine written in **Rust 2021**. The simulator executes embedded firmware workloads natively on the host CPU without QEMU-style instruction emulation, without simulated MCU binaries, and without using host OS threads as the RTOS task abstraction.

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
universal-rtos-sim/
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

