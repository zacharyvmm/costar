# costar architecture

costar runs unmodified RTOS firmware (FreeRTOS, Zephyr) deterministically, in
virtual time, on a single host thread. It can run many machines side by side.

The previous implementation is at
[zacharyvmm/costar_legacy](https://github.com/zacharyvmm/costar_legacy). §11
explains why costar was restarted.

**Status**

| Part | State |
|---|---|
| Virtual CPU, firmware images, World loop | implemented |
| FreeRTOS port | implemented |
| Zephyr runner | implemented |
| Devices, wires, plants | implemented |
| Scenarios, CLI | implemented |
| Sessions, gRPC server | implemented |

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

Devices come in three shapes. The board (set up by the host) names each one
and gives it an IRQ line. Firmware finds a device with `costar_dev(name)` and
its IRQ with `costar_dev_irq(dev)`.

| Shape | Covers | Firmware functions |
|---|---|---|
| **Channel**: frames in and out | UART, CAN, Ethernet, BLE HCI, SPI, I2C | `costar_chan_send`, `costar_chan_recv`. An arriving frame raises the device's IRQ. |
| **Registers**: u32 values by index | GPIO, ADC, sensors, entropy | `costar_reg_read`, `costar_reg_write`. A value changed by the host raises the IRQ. |
| **Memory**: bytes; erased bytes read 0xFF | flash, EEPROM, block devices | `costar_mem_size/read/write/erase` |

Devices are plain data (`src/costar/devices.*`). Behaviour lives elsewhere:
wires move frames between machines, and plants model the physical world.

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

## 6. Wires, plants and host events

- **Wires** (`src/costar/wires.*`) connect channel devices. A frame sent by one
  endpoint reaches every other endpoint, so a two-endpoint wire is a link and
  a larger one is a bus. A wire has:
  - a latency;
  - an optional speed in bits per second (frames then take time to send);
  - `shared_medium` for buses such as CAN, which carry one frame at a time
    (otherwise only each sender's frames are serialised);
  - fault windows that drop or corrupt frames sent during them.
- **Plants** (`src/costar/plant.hpp`) model the physical world. The world steps
  each plant periodically, and the plant reads actuators and writes sensors
  through the machines' devices (`Machine::set_register`, `Machine::deliver`).
- **Host events** (`src/costar/events.*`) run at virtual times: wire
  deliveries, plant steps, scenario injections. Events at the same time run
  in the order they were scheduled.

The trace records, per machine:

| Label | Meaning |
|---|---|
| `tx` / `rx` | a frame sent or received (`<device> <hex>`) |
| `drop` | a frame lost to a fault |
| `reg` | a firmware register write (`<device>[<reg>]=<value>`) |
| `set` | a register changed by the host or a plant |
| `mem` | memory written by the host (`<device>+<offset> <hex>`) |
| `log` | console text |
| `stop` | the CPU stopped |
| other | firmware `costar_trace` calls |

## 7. Scenarios and the `costar` command

A scenario file (TOML; `src/costar/scenario.*`) describes a world. It's parsed
into plain data (`Scenario`), then `build_world()` adds it to a `World`.

```toml
name = "link-nodes"
duration_ms = 30                      # omitted: run until nothing is left

[[machine]]
name = "responder"
image = "freertos_link_node.so"       # omitted: a passive machine
devices = [{ name = "link", kind = "channel", irq = 1 },
           { name = "node", kind = "registers" },
           { name = "log", kind = "memory", size = 64 }]

[[wire]]
endpoints = ["initiator.link", "responder.link"]
latency_ms = 2
bits_per_second = 115200              # optional, with bits_per_byte,
byte_stream = true                    # byte_stream and shared_medium
faults = [{ kind = "drop", from_ms = 5, until_ms = 15 }]

[[input]]                             # host actions at virtual times; exactly one of
at_ms = 0                             #   deliver / send / write (text, or *_hex)
target = "responder.node"             #   register + value
register = 0
value = 1

[[plant]]
type = "thermal"                      # a plant linked into the program
period_ms = 50
target = "thermostat.thermo"          # other keys are the plant's parameters

[expect]
trace = "link_nodes.trace"            # golden trace for `costar test`
```

Notes on the format:
- Parsing is strict. Unknown keys, wrong types and out-of-range values are
  errors, as are references to missing machines or devices, or to devices of
  the wrong kind. All of them are reported with the file and table at load
  time, not discovered mid-run.
- Times are `<key>_ms`, `<key>_us` or `<key>_ns`.
- Images are looked up next to the scenario, then in `--images` directories.
- Inputs at time 0 run before machines boot.

The `costar` command (`src/costar/cli.*`):

```
costar run  <scenario.toml> [--images DIR]... [--until-ms N] [--trace FILE]
costar test <scenario.toml | DIR>... [--images DIR]... [--update]
```

`test` compares each scenario's trace with its golden file and reports the
first differing line. `--update` rewrites the goldens.

Plants are compiled code, so the CLI is a library function,
`costar::cli::main(argc, argv, plants)`. `apps/costar/main.cpp` registers the
example `thermal` plant. A project with its own plants (microcar) writes the
same three lines with its own factories.

`tests/scenarios/` has two kinds of scenario:
- costar_legacy's scenarios, ported. Their timings match legacy's goldens,
  except the UART, which now uses the exact 10-bits-per-byte rate.
- Firmware scenarios: link nodes on a link and on a CAN-style bus with a fault
  window, and the thermostat with the thermal plant.
- With Zephyr enabled, also `tests/scenarios/zephyr/`.

## 8. Engine flow

The engine is three levels deep from `World::run_until` to firmware.

```cpp
// src/costar/world.cpp
void World::run_until(Time until) {
  while (const std::optional<Time> next = next_event_time()) {
    if (*next > until) break;
    now_ = *next;
    fire_events_at(now_);   // deliveries, plant steps, timers: device state and IRQs
    run_awake_machines();   // each awake CPU runs until it halts or polls
    route_outputs(now_);    // frames the firmware sent go onto wires
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

Within one instant, host events run first, then CPUs in machine-id order,
then outgoing frames are routed. A zero-latency delivery therefore runs in
another round at the same instant.

## 9. Layout

```
include/costar.h, costar_host.h     the C ABI and the image↔engine table
ports/common/                       image_stub.c, image.map (linked into every image)
ports/freertos/                     port.c, portmacro.h
ports/zephyr/                       runner.c (native_sim runner on costar)
src/costar/                         the engine (namespace costar)
  world.*     clock, machines, wires, plants, event loop
  machine.*   Cpu + Image + Devices; the firmware and host views of devices
  cpu.*       threads, IRQ lines, timers, interrupt delivery
  devices.*   Channel, Registers, Memory
  wires.*     links and buses: latency, speed, shared medium, faults
  events.*    host actions at virtual times
  plant.hpp   the Plant interface
  scenario.*  TOML scenario → Scenario → World
  session.*   an interactive simulation on its own thread (actor)
  cli.*       the costar command, as a library function
src/costar_grpc/                    the gRPC service over sessions (optional)
proto/costar/v1/costar.proto        the remote API
apps/costar/                        the costar executable, with the example thermal plant
apps/costar-grpc/                   the costar-grpc server
  image.*     load a private copy of a firmware image
  abi.*       the costar_host table → Machine/Cpu
  trace.*     records and formatting
  time.hpp    Time (ns), kNever
cmake/CostarFirmware.cmake          costar_add_freertos_image(), costar_add_zephyr_image()
firmware/                           test firmware (plain RTOS apps)
tests/                              doctest tests; scenarios/ with golden traces
third_party/FreeRTOS-Kernel         submodule (pinned)
```


## 10. Remote API: one server, one session model

There is one remote API, gRPC ([`proto/costar/v1/costar.proto`](../proto/costar/v1/costar.proto)).
JSON-RPC is dropped.

**`Session`** (`src/costar/session.*`) is in the engine library and doesn't
depend on gRPC:
- It's an actor. Its own thread builds and owns the `World`, and every public
  method is a command that thread runs between 1 ms slices of simulation.
  Nothing else touches the world, so there are no locks around it, no world
  checkouts, and no generation or revision counters.
- `run(until)` returns at once and the session runs in the background;
  `pause()` stops it. A world with nothing left to happen stops by itself.
- Running in slices gives exactly the trace of one uninterrupted `run_until`
  (tested).
- `input(...)` takes effect at a given virtual time (or now) and is recorded,
  together with the virtual time it was submitted at. So history is
  deterministic: `seek(t)` rebuilds the world and replays the scenario plus
  recorded inputs up to `t`, backwards or forwards.
  - Each input is queued once the replay reaches its submission time, so it
    lands in the same place among the world's own events as it did the first
    time.
  - Inputs after `t` are dropped.
  - A seek that fails leaves the world and its history untouched.
- `close()` stops a session at once, even while others still hold it.
  `DestroySession` closes the session and cancels its `Watch` calls.
- Trace readers hold a `TraceCursor`. A seek invalidates cursors started
  before it, so a reader can never mix two histories.
- If the simulation throws (a plant, say), the session stops and reports the
  error in its status.

**`costar-grpc`** (`src/costar_grpc/server.*`, built with
`-DCOSTAR_BUILD_GRPC=ON`) is a registry of sessions plus message translation:

| RPCs | Purpose |
|---|---|
| `CreateSession`, `DestroySession`, `ListSessions` | session lifecycle |
| `Run`, `Pause`, `Status` | running |
| `Input`, `Seek` | changing the world and its history |
| `Inspect` | machines, CPU states, devices, registers, memory |
| `Watch` | the trace from an index, then live; ends with `ABORTED` after a seek |

Errors map to status codes: unknown session `NOT_FOUND`, bad input
`INVALID_ARGUMENT`, bad scenario or firmware `FAILED_PRECONDITION`.

Like the CLI, the server is a library function taking the program's plants.

## 11. Why costar was restarted

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

## 12. Build order

1. ✅ **FreeRTOS on the virtual CPU.**
   - Port, CPU, images, World.
   - Unmodified ping-pong app, two independent copies of one image, tickless
     and non-tickless idle, deterministic reruns, repeated mid-run teardown.
2. ✅ **Zephyr on the virtual CPU.**
   - native_sim runner; unmodified Zephyr app matches stock native_sim's
     output.
   - Two independent copies, mixed Zephyr + FreeRTOS worlds, mid-run teardown.
3. ✅ **Devices, wires, plants.** Interrupt-driven FreeRTOS drivers (ISR →
   `vTaskNotifyGiveFromISR`), links and buses with timing and faults, a
   plant closing a control loop through registers, memory logging.
4. ✅ **Scenarios and CLI.** TOML scenarios, `costar run` / `costar test`
   with golden traces, and legacy's scenarios ported.
5. ✅ **Sessions and gRPC.** Actor sessions with deterministic seek, the
   `costar.v1` API, and an end-to-end test whose streamed trace equals the
   scenario's golden file.
6. **Zephyr drivers** for costar devices (an out-of-tree module), then the
   microcar migration.
7. **Host bridges** (TAP, TCP) as opt-in, non-deterministic wires.

## 13. Build and dependencies

- C++20, C11, CMake ≥ 3.28. Ninja and ccache are recommended.
- **Boost.Context**:
  - Found via `find_package(Boost CONFIG COMPONENTS context)`.
  - Otherwise fetched: Boost 1.90 CMake tarball, about 100 MB once, building
    only Boost.Context.
- **doctest** (tests) and **toml++** (scenarios, header-only, private to
  `scenario.cpp`) are fetched.
- **gRPC and protobuf** (optional, `-DCOSTAR_BUILD_GRPC=ON`) come from the
  system. CI uses Ubuntu's packages. Only `costar_grpc` includes their headers,
  so engine edits don't recompile them.
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
