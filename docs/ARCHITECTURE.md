# costar architecture

costar runs unmodified RTOS firmware (FreeRTOS, Zephyr) deterministically, in
virtual time, on a single host thread. It can run many machines side by side.

The previous implementation is at
[zacharyvmm/costar_legacy](https://github.com/zacharyvmm/costar_legacy). §11
explains why costar was restarted, and §14 maps legacy's features onto this
one.

**Status**

| Part | State |
|---|---|
| Virtual CPU, firmware images, World loop | implemented |
| FreeRTOS port | implemented |
| Zephyr runner | implemented |
| Devices, wires, plants | implemented |
| Scenarios, CLI | implemented |
| Sessions, gRPC server | implemented |
| Zephyr drivers for costar devices | implemented (UART, flash) |
| Host bridges (UDP, TCP, TAP) | implemented |
| Expectations, power cycling, frame-matched faults, busy-loop preemption, session clones | implemented |

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

**Stalls and hangs.** Firmware that keeps running without letting time pass
would stop the world. There are two ways to do that, and costar catches both:
- **A stall:** a loop that makes costar calls, such as switching threads,
  polling a register or taking a lock. Every firmware call passes through
  `Machine::count_call()`. If a machine makes more than the stall limit at one
  instant (default 1,000,000, about 0.1 s of wall-clock time), costar stops
  its CPU, as `costar_cpu_stop()` does, and records `stall`. The world goes
  on, and `costar run` and `costar test` fail with the machine's name and the
  time. It's deterministic: the same firmware stalls at the same point every
  run. A scenario can change the limit with `stall_limit`, where 0 means no
  limit (`World::set_stall_limit`).
- **A hang:** a loop that makes no calls at all (`while (!flag) {}`) never
  gives control back, so only another thread can notice it. `--timeout-s N`
  gives each scenario a wall-clock budget. When it runs out, the command
  writes what it has so far (the trace, or the scenario's `FAIL` line) and
  exits with status 1.

Both are bugs on costar's CPU, though not always on hardware. The fix is the
same either way: wait on an interrupt, delay, or build the code with
`INSTRUMENT` (§5).

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
| a task function that returns | `configASSERT( pdFALSE )`, as in the upstream ports. Without `configASSERT`, the task is deleted (or, without `INCLUDE_vTaskDelete`, the machine stops). |
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

`firmware/freertos_kernel_probe` checks the port against the FreeRTOS
behaviours costar_legacy got wrong: tasks created at runtime (with their
parameter), static tasks, software timers, a task that returns (a failed
assertion, not a silently stopped CPU), an IRQ raised inside a critical
section (it runs at unmask, and the task it wakes preempts), an IRQ between
ticks (it runs on arrival), deleting a blocked task, and a failed
`configASSERT` (the test config traces it and stops the machine).

**Busy loops.** On an infinitely fast CPU a busy loop (`while (!ready) {}`)
never lets time pass, so it stops the world. `costar_add_freertos_image(...
INSTRUMENT)` compiles the application's sources (not the kernel) with
`-fsanitize-coverage=trace-pc`. [`ports/common/busy_poll.c`](../ports/common/busy_poll.c)
then charges 10 ns per basic block and calls `costar_cpu_poll()` every 1000
blocks, so the tick and device interrupts preempt the loop
(`tests/scenarios/busy_loop.toml`). GCC and Clang both support the flag.
Uninstrumented code can call `costar_cpu_poll()` itself.

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
| `nsi_host_*` (libc trampolines) | upstream's own `nsi_host_trampolines.c`, linked into the image |

The test app's console output matches stock native_sim's (`zephyr.exe`) line
for line, at the same virtual times.

ztest suites run unmodified. `tests/scenarios/zephyr/ztest.toml` runs one of
kernel primitives (semaphores, mutexes, message queues, delayable work,
timers, aborting a thread). Its output and timing match stock native_sim's,
and ztest ends the run with the suite's result: `stop 0`.

Zephyr's timer core never disarms its compare: with no timeouts it arms one
far in the future. So an idle Zephyr machine is halted, not quiescent, and
`World::run()` will skip ahead to that far timeout.

**Zephyr drivers** ([`ports/zephyr/module`](../ports/zephyr/module)) are an
out-of-tree Zephyr module. `costar_add_zephyr_image()` adds it to every image,
together with its `costar` shield. That's added code, not a patch.
- `costar,uart` puts a channel behind Zephyr's UART API (polling and
  interrupt-driven).
- `costar,flash` puts a memory behind the flash API.
- Their ISRs are Zephyr ISRs on costar IRQ lines (3–31).
- The shield disables native_sim's host-backed devices (PTY UARTs, file-backed
  flash and EEPROM, host RNG, SDL, TAP, …), which need Zephyr's own runner.

Only the embedded part of the Zephyr build is built: `zephyr_pre0`, or
`zephyr_pre1` when `CONFIG_USERSPACE` or `CONFIG_DEVICE_DEPS` adds a link
stage. Zephyr's own runner executable can't link an image that uses costar
drivers, and costar doesn't use it.

## 6. Wires, plants and host events

- **Wires** (`src/costar/wires.*`) connect channel devices. A frame sent by one
  endpoint reaches every other endpoint, so a two-endpoint wire is a link and
  a larger one is a bus. A wire has:
  - a latency;
  - an optional speed in bits per second (frames then take time to send);
  - `shared_medium` for buses such as CAN, which carry one frame at a time
    (otherwise only each sender's frames are serialised);
  - fault windows that drop, corrupt or delay frames sent during them,
    optionally only frames that start with given bytes (a CAN ID, say). A
    delayed frame can arrive after later ones.
- **Plants** (`src/costar/plant.hpp`) model the physical world. The world steps
  each plant periodically, and the plant reads actuators and writes sensors
  through the machines' devices (`Machine::set_register`, `Machine::deliver`).
- **Host events** (`src/costar/events.*`) run at virtual times: wire
  deliveries, plant steps, scenario injections. Events at the same time run
  in the order they were scheduled.

- **Power.** `Machine::power_off()` discards the CPU (threads, timers,
  pending IRQs) and unloads the image, so RAM is lost. Frames queued on its
  channels are lost too, and frames that arrive while it is off are dropped.
  Both losses are recorded as `drop`. Registers and memory keep their
  contents: they model sensors and flash. `power_on()` boots a fresh private
  copy of the image. A reboot with downtime is an `off` input, then an `on`
  input later. A machine whose firmware stopped itself (an assertion,
  `nsi_exit`) is still on: `on` does nothing, and `reset` reboots it.
  `Inspect` shows such a CPU as `stopped`, and a powered-off one as `off`.

- **Host bridges** (`src/costar/host.*`) connect a channel to the real host:
  - **UDP:** each datagram is a frame.
  - **TCP:** frames with a 2-byte length prefix, listening or connecting.
  - **Linux TAP:** Ethernet frames.

  Frames the firmware sends go out at once; TCP output to a slow peer is
  queued, so a host never stalls the simulation. Frames from the host are
  delivered at the current virtual time when the world polls its links. That
  makes bridges **not deterministic**, so they're opt-in:
  - `World::run_realtime(until)` keeps virtual time in step with the wall
    clock and polls host links;
  - real-time sessions run each 1 ms slice once the wall clock reaches its
    end, sleeping in between, and refuse to seek, since host traffic isn't
    recorded;
  - `costar run` runs bridged scenarios in real time;
  - `costar test` skips bridged scenarios.

The trace records, per machine:

| Label | Meaning |
|---|---|
| `tx` / `rx` | a frame sent or received (`<device> <hex>`) |
| `drop` | a frame lost: to a fault, to or from a machine that is off, or queued when it was switched off |
| `reg` | a firmware register write (`<device>[<reg>]=<value>`) |
| `set` | a register changed by the host or a plant |
| `mem` | memory written by the host (`<device>+<offset> <hex>`) |
| `log` | console text |
| `stop` | the CPU stopped |
| `stall` | costar stopped the CPU: too many calls without time passing (§2) |
| `power` | the host switched the machine off (0) or on (1) |
| other | firmware `costar_trace` calls |

## 7. Scenarios and the `costar` command

A scenario file (TOML; `src/costar/scenario.*`) describes a world. It's parsed
into plain data (`Scenario`), then `build_world()` adds it to a `World`.

```toml
name = "link-nodes"
duration_ms = 30                      # omitted: run until nothing is left
stall_limit = 1000000                 # optional: firmware calls per instant (§2); 0: none

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
faults = [{ kind = "drop", from_ms = 5, until_ms = 15 },
          { kind = "delay", delay_ms = 5, match_hex = "52 03" }]  # drop | corrupt | delay

[[input]]                             # host actions at virtual times; exactly one of
at_ms = 0                             #   deliver / send / write (text, or *_hex)
target = "responder.node"             #   register + value
register = 0                          #   power = "off" | "on" | "reset" (target: a machine)
value = 1

[[plant]]
type = "thermal"                      # a plant linked into the program
period_ms = 50
target = "thermostat.thermo"          # other keys are the plant's parameters

[[bridge]]                            # optional: connect a channel to the host
endpoint = "responder.link"           # (makes the scenario real-time)
kind = "udp"                          # udp (local, remote) | tcp-listen | tcp-connect (address)
local = "127.0.0.1:9400"              # | tap (interface)

[expect]
trace = "link_nodes.trace"            # golden trace for `costar test`

[[expect.event]]                      # a trace record that must appear
machine = "responder"                 # optional: any machine
label = "got_ping"
value = 3                             # optional: the record's value (a frame's size for rx/tx), or text = "..."
before_ms = 22                        # optional window: after_*, before_* (inclusive)

[[expect.no]]                         # a record that must not appear
label = "got_ping"
value = 2
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
costar run  <scenario.toml> [--images DIR]... [--until-ms N] [--trace FILE] [--realtime] [--timeout-s N]
costar test <scenario.toml | DIR>... [--images DIR]... [--update] [--timeout-s N]
```

`run` writes the trace as the run goes. With `--trace FILE`, it goes into a
file of costar's own, which is renamed into place when the run ends: even a
run that fails or times out leaves its trace so far. `test` prints each
scenario's result as soon as it has it.

`test` runs each scenario that has a golden trace or expectations. It
compares the trace with the golden file, reporting the first differing line,
and checks every expectation. `--update` rewrites the goldens; expectations
are still checked. `run` prints the trace and fails if an expectation does.

Plants are compiled code, so the CLI is a library function,
`costar::cli::main(argc, argv, plants)`. `apps/costar/main.cpp` registers the
example `thermal` plant. A project with its own plants (microcar) writes the
same three lines with its own factories.

**In CI**, `costar_add_scenario_tests()`
([`cmake/CostarScenarios.cmake`](../cmake/CostarScenarios.cmake)) registers
each scenario file as its own ctest, `<prefix>/<name>`, which runs
`costar test` on that file with `--timeout-s`:
- `ctest -j` runs scenarios in parallel.
- Firmware that crashes the process takes only its own scenario down.
- `ctest --output-junit` writes a JUnit report.
- Scenarios that `costar test` skips are reported as skipped.
- A program with its own plants passes its executable as `COMMAND`.

costar's own tests use it, and CI uploads the JUnit report.

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
ports/common/                       image_stub.c, image.map (linked into every image);
                                    busy_poll.c (INSTRUMENT images)
ports/freertos/                     port.c, portmacro.h
ports/zephyr/                       runner.c (native_sim runner on costar)
ports/zephyr/module/                Zephyr drivers for costar devices, and the costar shield
src/costar/                         the engine (namespace costar)
  world.*     clock, machines, wires, plants, event loop
  machine.*   Cpu + Image + Devices; the firmware and host views of devices
  cpu.*       threads, IRQ lines, timers, interrupt delivery
  devices.*   Channel, Registers, Memory
  wires.*     links and buses: latency, speed, shared medium, faults
  events.*    host actions at virtual times
  plant.hpp   the Plant interface
  host.*      host links (UDP, TCP, TAP) for bridges
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
cmake/CostarScenarios.cmake         costar_add_scenario_tests(): one ctest per scenario
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
- `run(until, realtime)` returns at once and the session runs in the
  background, as fast as it can or in step with the wall clock (serving host
  bridges). `pause()` stops it. A world with nothing left to happen stops by
  itself, unless it's real-time.
- Running in slices gives exactly the trace of one uninterrupted `run_until`
  (tested).
- `input(...)` takes effect at a given virtual time (or now) and is recorded,
  together with the point in the run it was submitted at: the number of world
  steps taken by then. A step handles one instant, and the sequence of steps
  doesn't depend on how a run is sliced. So history is deterministic:
  `seek(t)` rebuilds the world and replays the scenario plus recorded inputs
  up to `t`, backwards or forwards.
  - Each input is queued after exactly as many steps as before, so it lands
    in the same place among the world's own events as it did the first time,
    even among events at the same instant.
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
| `CloneSession` | a new, paused session with the same history at the same time: a branch |
| `Run`, `Pause`, `Status` | running |
| `Input`, `Seek` | changing the world and its history (inputs include power) |
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
6. ✅ **Zephyr drivers** for costar devices: an out-of-tree module (UART,
   flash) and the `costar` shield. The test app uses Zephyr's UART interrupts
   and flash APIs on costar devices.
   - Still to do, in the microcar repository: the **microcar migration**.
     Firmware calls `costar.h` devices instead of `sim_*`, the Rust plant is
     ported to a C++ `Plant`, and scenarios become TOML.
7. ✅ **Host bridges.** UDP, TCP (listen or connect) and TAP links, real-time
   runs and sessions. Tested with real sockets against interrupt-driven
   firmware.
8. ✅ **costar_legacy's remaining features** (§14): scenario expectations,
   machine power cycling, delay and frame-matched faults, busy-loop
   preemption, ztest, and session clones. Regression probes for the bugs that
   legacy's last PRs found.

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

## 14. costar_legacy's features, and where they are now

| costar_legacy | costar |
|---|---|
| FreeRTOS tasks, queues, delays, timers, semaphores, event groups, notifications, deletion, static tasks | The unmodified kernel, all of it. `firmware/freertos_kernel_probe` checks what legacy got wrong. |
| Zephyr threads, `k_sem`, `k_mutex`, `k_msgq`, `k_work`, `k_timer`, ztest | Unmodified native_sim builds, ztest included (`tests/scenarios/zephyr/ztest.toml`). |
| UART, GPIO, timer, I2C, SPI, CAN, ADC, temperature sensor, EEPROM, flash, IRQ controller | Three device shapes: channels (UART, CAN, SPI, I2C, Ethernet, BLE HCI), registers (GPIO, ADC, sensors) and memory (EEPROM, flash, block devices). Timers and IRQ lines belong to the CPU. Zephyr drivers: UART, flash. |
| Fault injector: I2C NACK, SPI and CAN corruption, UART framing errors, GPIO stuck-at | Wire faults (drop, corrupt, delay), by time window and frame prefix. Stuck inputs are register inputs or plants. |
| Scenario faults: `reboot` with downtime, `drop_frame` / `delay_frame` by CAN ID | `power` inputs; wire faults with `match`. |
| Scenario faults: `force_temperature`, `stop_heartbeat` | Plant parameters and register inputs: they were microcar's, not costar's. |
| Injections: `ble_event`, `block_data`, `bus_inject` | `deliver`, `write` and `send` inputs. |
| `[[expect.event]]`, `[[expect.no]]`, golden traces | The same, matched on trace labels (§7). |
| Instrumentation tiers 1–3 | One mechanism: `INSTRUMENT` (§5), plus `costar_cpu_poll()`. |
| Multi-machine World, links, CAN bus, plants, per-machine RTOS | World, wires (`shared_medium` for buses), plants, an image per machine. |
| TCP bridge, TAP bridge, host I/O | Host bridges: UDP, TCP, TAP (§6). |
| JSON-RPC and gRPC servers, keyframes, reset, clone | One gRPC API. `Seek` replays deterministically, so keyframes and reset aren't needed; `CloneSession`. |
| GUI display frames and touch events | Memory and channel devices, seen through `Inspect` and `Watch`. There's no framebuffer-specific RPC yet. |

Not ported:
- **FreeRTOS+TCP and the smoltcp bridge.** Firmware can exchange Ethernet
  frames over channels, wires and TAP bridges, but costar has no
  `NetworkInterface` for FreeRTOS+TCP yet.
- **A deterministic entropy device.** Neither costar's firmware nor
  microcar's uses one. Firmware can seed its own generator from a register.
- **CLI extras:** `costar shell` and `replay` (sessions and `Seek` cover
  them), the JSON-RPC server and its Go client (dropped for gRPC), JSONL
  output and `--machine-filter`. (`--watchdog` is now stall detection and
  `--timeout-s`, §2.)
- **Windows and macOS.** costar is built and tested on Linux only.
