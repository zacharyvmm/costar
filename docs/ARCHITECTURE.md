# costar architecture

costar runs unmodified RTOS firmware (FreeRTOS, Zephyr) deterministically, in
virtual time, on a single host thread. It can run many machines side by side.

The previous implementation is at
[zacharyvmm/costar_legacy](https://github.com/zacharyvmm/costar_legacy). §9
explains why costar was restarted.

**Status**

| Part | State |
|---|---|
| Virtual CPU, firmware images, World loop | implemented |
| FreeRTOS port | implemented |
| Zephyr runner | implemented |
| Devices, wires, plants, scenarios | planned |
| gRPC server | planned |

## 1. The core idea: costar is a virtual CPU

Each RTOS runs on costar through its normal, upstream port contract. costar
never patches RTOS sources and never makes scheduling decisions.

```
  firmware image (one shared object per build)
  ┌──────────────────────────────────────────────────────────────────────┐
  │ application + unmodified RTOS kernel                                 │
  │ RTOS port:  ports/freertos/port.c  |  ports/zephyr (native_sim runner)│
  │ ports/common/image_stub.c   implements costar.h via a function table │
  └───────────────────────────────┬──────────────────────────────────────┘
                                  │ costar_host table + this machine's pointer
  ┌───────────────────────────────┴──────────────────────────────────────┐
  │ Machine = Cpu (fibers, IRQ lines, timers) + Image (+ Devices)        │
  └───────────────────────────────┬──────────────────────────────────────┘
  ┌───────────────────────────────┴──────────────────────────────────────┐
  │ World = machines (+ wires, plant) sharing one clock and one Trace    │
  └──────────────────────────────────────────────────────────────────────┘
```

## 2. Execution model: an infinitely fast CPU

This is the same model Zephyr's `native_sim` uses.

1. Firmware runs only inside `Cpu::run()`. Virtual time does not move while it
   runs.
2. The RTOS decides everything about threads. To switch, the port calls
   `costar_thread_switch(next)`, and the CPU trampoline resumes `next`.
3. When the RTOS has nothing to run, its idle path calls `costar_cpu_halt()`
   (WFI). The World then takes over.
4. The World moves time to the earliest event: a timer, the end of a busy
   period, and later wire deliveries and plant ticks. Due timers raise IRQ
   lines, and a raised IRQ wakes a halted CPU.
5. Interrupts are delivered at **interrupt points**:
   - when `costar_cpu_halt()` or `costar_cpu_poll()` returns;
   - when `costar_irq_unlock()` re-enables interrupts.

   Each pending IRQ runs the port's ISR entry on the current thread, as
   hardware does. If any ISR asks for a switch, costar then calls the port's
   context-switch function, the equivalent of PendSV on Cortex-M.
6. `costar_cpu_poll(cost)` is a preemption point for instrumented busy loops.
   It charges `cost` of virtual time and lets interrupts in.

## 3. The C ABI

Firmware sees [`include/costar.h`](../include/costar.h):

| Area | Functions |
|---|---|
| CPU | `costar_now`, `costar_cpu_init(isr, switch_context)`, `costar_cpu_halt`, `costar_cpu_poll`, `costar_cpu_stop(status)` |
| Interrupts | `costar_irq_lock`, `costar_irq_unlock` |
| Timers | `costar_timer_set(irq, at)`, one-shot |
| Threads | `costar_thread_new`, `costar_thread_switch`, `costar_thread_free` |
| Observability | `costar_trace(label, value)`, `costar_log(text, length)` for console text |

Virtual time is `uint64_t` nanoseconds.

Device functions arrive with step 3 of the build order (§10). There are three
device shapes:

| Shape | Covers |
|---|---|
| **Channel**: frames in and out, RX IRQ | UART, CAN, Ethernet, BLE HCI, SPI, I2C |
| **Registers**: u32 values | GPIO, ADC, sensors, entropy |
| **Memory**: bytes with erase geometry | flash, EEPROM, block devices, framebuffer |

### How an image reaches its machine (`include/costar_host.h`)

Each image is linked with `ports/common/image_stub.c`. That stub implements
every `costar_*` function by calling through a `costar_host` function table,
passing a `machine` pointer.

Right after loading an image, costar calls `costar_image_attach(&table, machine)`.
Every loaded copy has its own globals, so every copy forwards to its own
machine.

Each image also exports a reset vector, `costar_image_reset()`, provided by
its RTOS port. costar runs it on the CPU's first thread:
- The FreeRTOS port's reset vector calls the application's `main()`.
- The Zephyr runner's boots Zephyr. A Zephyr image has its own `main()`, the
  application's, so `main` can't be the entry point.

The result is no thread-locals, no "active machine" stacks, and no global
state in the engine.

## 4. Firmware images and multiple machines

An image is a shared object containing the application, the kernel, the port
and the stub. It's built with `costar_add_freertos_image()` or
`costar_add_zephyr_image()` in
[`cmake/CostarFirmware.cmake`](../cmake/CostarFirmware.cmake). A version script
([`ports/common/image.map`](../ports/common/image.map)) exports only
`costar_image_reset` and `costar_image_attach`, so all internal references
bind inside the image.

`Image` (`src/costar/image.cpp`) loads a **private copy** of the file per
machine, using `mkstemp` + copy + `dlopen(RTLD_LOCAL)`. It does this because
`dlopen` returns the already-loaded object for a file it has seen. Two
machines running the same firmware therefore get two independent kernels,
with no snapshotting of kernel statics.

`tests/freertos_test.cpp` checks this. A mutation that loads the original
file directly makes the two-copies test abort.

## 5. RTOS adapters

### FreeRTOS: `ports/freertos/{port.c, portmacro.h}`

This has the shape of the upstream `ThirdParty/GCC/Posix` port. The application
keeps its own `FreeRTOSConfig.h`.

| Port contract | costar implementation |
|---|---|
| `pxPortInitialiseStack` | `costar_thread_new`. The handle is stored at the top of the task stack and found through the TCB's first member, as the Posix port does. |
| `portYIELD` | `vTaskSwitchContext()`, then `costar_thread_switch` to the task FreeRTOS chose. Inside a critical section the switch is deferred to the section's end (PendSV semantics). |
| critical sections, interrupt masks | `costar_irq_lock/unlock`, with a nesting count |
| tick interrupt | a one-shot timer on `configCOSTAR_TICK_IRQ` (default 0), re-armed by the tick ISR. The ISR catches up on missed ticks. |
| other interrupts | `vPortSetInterruptHandler(irq, handler)`. Handlers use the normal `...FromISR` APIs and `portYIELD_FROM_ISR`. |
| `portSUPPRESS_TICKS_AND_SLEEP` (tickless idle) | Halt until the next timeout. With no timeout at all, halt with the tick timer off, so a quiescent machine generates no events. |
| `portCLEAN_UP_TCB` | `costar_thread_free`, which discards the deleted task's stack |
| `configCONTROL_INFINITE_LOOP()` | See below. |

The idle hook: FreeRTOS only calls the port from idle when it can sleep for 2+
ticks. Otherwise the idle loop spins, and on a cooperative CPU time would
never advance. `configCONTROL_INFINITE_LOOP()` is evaluated once per idle-loop
iteration, so the port uses it to yield to other idle-priority tasks and then
halt until the next interrupt. It skips the halt when the previous iteration
already did a tickless sleep.

Nothing in the kernel is patched, and applications make no costar calls apart
from `costar_trace`. `firmware/freertos_ping_pong` is a plain FreeRTOS app. Its
trace shows real preemption: the higher-priority receiver records "received n"
before the sender records "sent n".

Requirements on the app's config:
- `configNUMBER_OF_CORES` 1;
- `INCLUDE_xTaskGetIdleTaskHandle` 1, which is the port's default when unset;
- `configUSE_TICKLESS_IDLE` 1 is recommended.

### Zephyr: an unmodified native_sim build, with costar as the runner

Zephyr's `native_sim` board already splits firmware into two parts:
- the *embedded image*: kernel, application, board and arch code. An ordinary
  native_sim build leaves it as the relocatable object `zephyr/zephyr.elf`.
- a *native simulator runner*, linked in afterwards.

[`ports/zephyr/runner.c`](../ports/zephyr/runner.c) is a runner implemented on
`costar.h`. `costar_add_zephyr_image()` runs the application's normal Zephyr
build (board `native_sim/native/64`), then links `zephyr.elf` with
`runner.c` and `image_stub.c` into an image. Zephyr is not patched, needs no
extra Kconfig, and keeps its generated headers inside its own build.

Two details of the build:
- The image is linked with the extra inputs Zephyr's own runner would have
  used, read from the build's `zephyr/NSI/nsi_config` (for example
  `-lstdc++` for C++ applications).
- The Zephyr build is reconfigured whenever the helper's arguments change.

The image leaves 38 functions and one variable undefined (Zephyr v4.5.0); the
runner provides all of them:

| Interface | costar implementation |
|---|---|
| `nct_*` threads | `costar_thread_new/switch/free`. A thread that aborts itself is freed once it has switched away. |
| `nce_*` CPU | Boot runs Zephyr on the reset thread. `nce_halt_cpu` = `costar_cpu_halt`. Wake-up is costar's job. |
| `hw_irq_ctrl_*` interrupt controller | The upstream model (masks, lock, priorities). costar's IRQ lines 0–31 are Zephyr's IRQs. When the CPU wakes, raised lines become pending, and the board's `posix_irq_handler()` runs Zephyr's ISRs and any context switch. |
| `hwtimer_*` timer | The tick one-shot is a costar timer on IRQ 0. The busy-wait "wake at" timer uses its own line and raises no Zephyr IRQ. |
| `nsi_simu_time` | Zephyr reads it directly (µs). It's refreshed whenever the CPU wakes. |
| `nsi_print*`, `nsi_exit` | `costar_log`; exit runs the image's cleanup (`ON_EXIT` native tasks), then `costar_cpu_stop` |

The test app's console output matches stock native_sim's (`zephyr.exe`) line
for line, at the same virtual times.

Zephyr's timer core never disarms its compare: with no timeouts it arms one
far in the future. So an idle Zephyr machine is halted, not quiescent, and
`World::run()` will skip ahead to that far timeout.

## 6. Engine flow

The engine is three levels deep from `World::run_until` to firmware.

```cpp
// src/costar/world.cpp
void World::run_until(Time until) {
  while (const std::optional<Time> next = next_event_time()) {
    if (*next > until) break;
    now_ = *next;
    fire_events_at(now_);   // timers raise IRQs; busy CPUs finish
    run_awake_machines();   // each awake CPU runs until it halts or polls
  }
  if (until != kNever) now_ = std::max(now_, until);
}

// src/costar/cpu.cpp — the trampoline; no scheduler
Cpu::State Cpu::run() {
  while (state_ == State::Awake) {
    running_->context = jump_fcontext(running_->context, running_).fctx;
    switch (request_.kind) {
      case Request::Switch: running_ = request_.next; break;   // the RTOS chose
      case Request::Halt:   state_ = State::Halted; break;
      case Request::Poll:   state_ = State::Busy; busy_until_ = now_ + request_.cost; break;
      case Request::Exit:   state_ = State::Exited; break;
    }
  }
  return state_;
}
```

Threads use Boost.Context's `fcontext` primitives on
`protected_fixedsize_stack`s. Each stack is at least 64 KiB and has a guard
page.

Freeing a thread discards its stack without unwinding it, as deleting a thread
does on real hardware. `errno` belongs to the host thread, so the CPU saves and
restores it around every switch: each RTOS thread keeps its own, as it would
with per-thread storage on a target. That also means firmware needs no unwind tables. Zephyr
builds don't emit them, and Boost's higher-level `fiber` (which unwinds on
destruction) aborted on them.

`Machine` declares its `Image` before its `Cpu`, so the code outlives the
threads running it.

## 7. Layout

```
include/costar.h, costar_host.h     the C ABI and the image↔engine table
ports/common/                       image_stub.c, image.map (linked into every image)
ports/freertos/                     port.c, portmacro.h
ports/zephyr/                       runner.c (native_sim runner on costar)
src/costar/                         the engine (namespace costar)
  world.*     clock, machines, event loop
  machine.*   Cpu + Image (+ Devices)
  cpu.*       fibers, IRQ lines, timers, interrupt delivery
  image.*     load a private copy of a firmware image
  abi.*       the costar_host table → Machine/Cpu
  trace.*     records and formatting
  time.hpp    Time (ns), kNever
cmake/CostarFirmware.cmake          costar_add_freertos_image(), costar_add_zephyr_image()
firmware/                           test firmware (plain RTOS apps)
tests/                              doctest tests
third_party/FreeRTOS-Kernel         submodule (pinned)
```

Planned modules:
- `devices.*`, `wires.*` (links and CAN bus with faults), `plant.*`,
  `scenario.*` (TOML → World), `events.*`;
- `costar-cli` (`run`, `test`);
- `costar-grpc` (session actor; built as a separate executable so engine
  edits never rebuild gRPC).

## 8. Remote API: one server, one session model

There will be gRPC only; JSON-RPC is dropped.

Each session is an actor: one thread owns its `World`, and RPCs arrive as
commands that it handles between `run_until` slices. That means no locks, no
world checkouts, and no generation or revision counters.

Runs are deterministic, so a keyframe is "rebuild, then replay the recorded
inputs up to `t`".

## 9. Why costar was restarted

In costar_legacy:
- FreeRTOS `tasks.c` and `timers.c` were string-patched at build time, and Rust
  picked which task ran by writing `pxCurrentTCB`. `vTaskSwitchContext` was
  never called.
- Firmware had to call `sim_create_task` and `sim_bridge_register`.
- Zephyr was rebuilt with the `cc` crate from a hand-picked file list plus ~80
  vendored generated headers.
- IRQ "delivery" only wrote trace events; no ISR ever ran.
- Five thread-local activation stacks tracked the "current machine".
- There were about six run loops and two RPC servers.

## 10. Build order

1. ✅ **FreeRTOS on the virtual CPU.**
   - Port, CPU, images, World.
   - Unmodified ping-pong app, two independent copies of one image, tickless
     and non-tickless idle, deterministic reruns, repeated mid-run teardown.
2. ✅ **Zephyr on the virtual CPU.**
   - native_sim runner; unmodified Zephyr app matches stock native_sim's
     output.
   - Two independent copies, mixed Zephyr + FreeRTOS worlds, mid-run teardown.
3. **Devices, wires, scenarios, CLI.** Port legacy's scenarios and demos as
   plain RTOS apps.
4. **gRPC session actor** for the GUI.
5. **Microcar migration**, and Zephyr drivers for costar devices (an
   out-of-tree module).
6. **Host bridges** (TAP, TCP) as opt-in, non-deterministic wires.

## 11. Build and dependencies

- C++20, C11, CMake ≥ 3.28. Ninja and ccache are recommended.
- **Boost.Context**:
  - Found via `find_package(Boost CONFIG COMPONENTS context)`.
  - Otherwise fetched: Boost 1.90 CMake tarball, about 100 MB once, building
    only Boost.Context.
- **doctest** is fetched for tests.
- **Zephyr images** (optional) need:
  - a Zephyr tree (`COSTAR_ZEPHYR_BASE`, defaulting to `$ZEPHYR_BASE`; tested
    with v4.5.0);
  - a Python with `scripts/requirements-base.txt` (`COSTAR_ZEPHYR_PYTHON`);
  - Ninja, or GNU Make as a fallback.

  No Zephyr SDK is needed: native_sim uses the host compiler. Without a Zephyr
  tree, Zephyr images and tests are skipped.

Measured on this machine (8 cores, GNU Make):

| Build | Time |
|---|---|
| Clean build, including Boost.Context and two firmware images | 4.4 s |
| Rebuild after editing an engine file | ~0.9 s |
| Rebuild after editing `port.c` | 0.1 s |
