# costar

Deterministic, virtual-time simulation of unmodified RTOS firmware on a single
host thread. Many machines can run side by side.

costar presents a small virtual CPU ([`include/costar.h`](include/costar.h)).
RTOSes run on it through their normal port contracts:
- **FreeRTOS:** an ordinary port in [`ports/freertos`](ports/freertos). No
  kernel patches.
- **Zephyr:** ordinary `native_sim` builds, with costar standing in for
  native_sim's runner ([`ports/zephyr`](ports/zephyr)).

The RTOS makes every scheduling decision. Read
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the design.

The previous implementation lives at
[zacharyvmm/costar_legacy](https://github.com/zacharyvmm/costar_legacy).

## Build and test

```bash
git submodule update --init
cmake -S . -B build
cmake --build build -j
ctest --test-dir build --output-on-failure
```

You need a C/C++20 toolchain and CMake ≥ 3.28. Boost.Context comes from the
system if it's installed; otherwise CMake fetches it.

Zephyr images are optional. To enable them, point CMake at a Zephyr tree and a
Python that has Zephyr's requirements installed:

```bash
python3 -m venv .venv && . .venv/bin/activate
pip install -r $ZEPHYR_BASE/scripts/requirements-base.txt ninja
cmake -S . -B build -DCOSTAR_ZEPHYR_BASE=$ZEPHYR_BASE
```

## Running scenarios

A scenario file describes machines (firmware images and devices), the wires
between them, timed inputs, and plants. See
[`tests/scenarios`](tests/scenarios) and the format in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md#7-scenarios-and-the-costar-command).

```bash
build/costar run tests/scenarios/link_nodes.toml --images build/firmware
build/costar test tests/scenarios --images build/firmware            # check goldens and expectations
build/costar test tests/scenarios --images build/firmware --update   # re-bless the goldens
```

`run` can narrow and reshape its trace:

```bash
build/costar run tests/scenarios/link_nodes.toml --images build/firmware \
    --machine responder --label 'got_*' --exclude-label tx   # filters, repeatable
build/costar run ... --format jsonl                          # JSON lines, for jq and scripts
build/costar run ... --format console                        # only the firmware's console text
build/costar run ... --perfetto trace.json                   # open in ui.perfetto.dev
```

When a trace doesn't match its golden file, `costar test` prints the first
hunks of a unified diff (`-` golden, `+` actual).

Runs are deterministic and cheap, so CI can explore timings and faults.
`--set KEY=VALUE` changes a scenario value for one run (`--set
wire.1.latency_ms=3`, `--set input.2.value=7`, or a slower CPU with `--set
machine.mcu.cpu.ns_per_block=20`). A `[[sweep]]` makes
`costar test` run every combination of some values. A failing combination
prints the `costar run ... --set ...` command that reproduces it. See
[`tests/scenarios/link_sweep.toml`](tests/scenarios/link_sweep.toml), and
[`tests/scenarios/zephyr/entropy_seeds.toml`](tests/scenarios/zephyr/entropy_seeds.toml)
for random seeds.

Besides a golden trace, a scenario can state what must (`[[expect.event]]`)
or must not (`[[expect.no]]`) happen, and when. Inputs can switch machines
off and on, and wire faults can drop, corrupt or delay matching frames. See
[`tests/scenarios/reboot.toml`](tests/scenarios/reboot.toml).

A machine can have a watchdog (`{ name = "wdt", kind = "watchdog" }`) that
its firmware must feed. If the firmware hangs, the watchdog reboots the
machine, and the trace records `watchdog`, so recovery paths get tested
([`tests/scenarios/watchdog.toml`](tests/scenarios/watchdog.toml)). Zephyr
firmware uses it through Zephyr's watchdog API (`watchdog0`).

Parts on a bus can be device models that answer the firmware, so real
drivers run unmodified: an `i2c-registers` model (a sensor), an
`i2c-eeprom` or a `spi-nor` flash keeps its state in a device of a passive
machine, where inputs set readings and the trace shows writes. See
[`tests/scenarios/zephyr/i2c.toml`](tests/scenarios/zephyr/i2c.toml), where
Zephyr's `ti,tmp112` and `atmel,at24` drivers read a temperature and log it
to an EEPROM, and [`spi.toml`](tests/scenarios/zephyr/spi.toml), where its
`jedec,spi-nor` driver erases, programs and reads a flash.

In CI, register scenarios as tests, one per scenario. Each one is its own
process, so `ctest -j` runs them in parallel and a crash in one doesn't take
the rest down. `ctest --output-junit` writes a report:

```cmake
include(path/to/costar/cmake/CostarScenarios.cmake)  # costar's own build includes it
costar_add_scenario_tests(scenarios
  SCENARIOS ${CMAKE_CURRENT_SOURCE_DIR}/scenarios    # files or directories
  IMAGES $<TARGET_FILE_DIR:my_freertos_app>
  TIMEOUT 60                                         # seconds per scenario
  ARTIFACTS ${CMAKE_BINARY_DIR}/scenario-artifacts)  # what failing scenarios leave
```

With `ARTIFACTS` (`costar test --artifacts DIR`), each failing scenario
leaves `DIR/<scenario>-<hash>/` behind (the hash tells apart scenarios
whose names look alike), even if it timed out. Upload it from CI
when tests fail:
- the whole trace, as text and for Perfetto;
- each machine's console text;
- the golden diff, and what the golden would be;
- `summary.txt`: what failed, and why.

`costar test --junit FILE` writes a JUnit report of its own, with each
failure's reason (a golden diff's first hunk, a failed expectation, a
timeout).

A golden trace can cover only what matters, so unrelated timing changes
don't churn it: `trace_machines`, `trace_labels` and `trace_exclude_labels`
in `[expect]` (see
[`tests/scenarios/link_nodes_pings.toml`](tests/scenarios/link_nodes_pings.toml)).
`--update` writes the filtered trace.

Firmware that never lets time pass can't hang CI:
- A machine that makes a million costar calls at one instant (a polling or
  thread-switching loop) is stopped as **stalled**, and the run fails.
- `--timeout-s N` gives each scenario a wall-clock budget, for loops that make
  no calls at all.

Firmware that overruns a thread's stack fails the run too. Threads run on
big host stacks, so costar checks each one against the size its RTOS gave it,
with an allowance for bigger host frames (`stack_check` in a scenario).
`costar run --stacks` reports each thread's stack use.

See [ARCHITECTURE.md §2](docs/ARCHITECTURE.md#2-execution-model-an-infinitely-fast-cpu).

## Zephyr test suites with twister

Zephyr's ztest suites run on costar under Zephyr's own test runner,
twister, unchanged. The platform is `native_sim/native/64/costar`, a board
in costar's Zephyr module: native_sim with the costar shield, whose
`zephyr.exe` runs the application on costar (`costar run --format
console`, in virtual time). twister's native handler runs it as usual.

```bash
export ZEPHYR_BASE=~/zephyrproject/zephyr ZEPHYR_TOOLCHAIN_VARIANT=host
M=$PWD/ports/zephyr/module
$ZEPHYR_BASE/scripts/twister -p native_sim/native/64/costar --board-root $M/boards \
    -x ZEPHYR_MODULES=$M -x COSTAR_EXECUTABLE=$PWD/build/costar \
    -T $ZEPHYR_BASE/tests/kernel/semaphore/semaphore
```

Inside a west workspace, pass `ZEPHYR_EXTRA_MODULES` instead of
`ZEPHYR_MODULES`. Suites limited to `native_sim` by `platform_allow` need
`--force-platform`. See
[`ports/zephyr/module/README.md`](ports/zephyr/module/README.md#running-test-suites-with-twister).

## Sanitizers and coverage

Firmware runs as host code, so the host's tools work on it:

```bash
cmake -S . -B build-asan -DCOSTAR_SANITIZE=address,undefined   # any finding fails the run
cmake -S . -B build-cov -DCOSTAR_COVERAGE=ON                    # gcov data for engine and images
```

Both options apply to costar's own targets and to every image built with
`costar_add_*_image()`. In your own project, also pass the program that
loads the images to `costar_instrument(<target>)`. With AddressSanitizer,
FreeRTOS images default to `heap_costar`, which makes each `pvPortMalloc()`
its own host `malloc()`, so heap bugs in firmware are caught too (it has
`heap_4`'s API and limit). CI runs
both builds and uploads the coverage report.

## Debugging

Firmware is host code, so GDB debugs it: breakpoints by function name work
before an image loads. `costar run --break-on [MACHINE:]LABEL[=VALUE]` stops
inside the firmware's call that records a matching trace record, and
`--break-at-ms N` stops at a virtual time. Runs are deterministic, so a
rerun stops at exactly the same point. `tools/gdb/costar.py` adds
`costar machines`, `costar threads` (the RTOS's threads, which GDB can't
see) and `costar bt <thread>`:

```bash
gdb -x tools/gdb/costar.py --args build/costar run tests/scenarios/link_nodes.toml \
    --images build/firmware --break-on responder:got_ping=2
```

See [`docs/DEBUGGING.md`](docs/DEBUGGING.md) for recipes and a VS Code
configuration.

## 32-bit builds

Most MCUs, Cortex-M included, are 32-bit: pointers, `long` and `size_t` have
4 bytes (ILP32). A normal build runs firmware as 64-bit host code (LP64),
which hides bugs that depend on those sizes: a truncated pointer, a `long`
assumed to hold 64 bits, a struct layout shared with other code, `size_t`
arithmetic that wraps. To run firmware with its target's sizes, build
everything as 32-bit x86:

```bash
sudo apt-get install gcc-multilib g++-multilib
cmake -S . -B build32 -DCMAKE_TOOLCHAIN_FILE=cmake/toolchains/linux-i686.cmake
```

Images are loaded into the engine, so the engine, the `costar` command, the
tests and the images are all 32-bit. Zephyr images are built for board
`native_sim` instead of `native_sim/native/64`. CI runs the whole suite this
way, and [`tests/scenarios/data_model`](tests/scenarios/data_model) checks
that images really are ILP32. Struct layout can still differ from ARM's:
x86 aligns 8-byte members to 4 inside structs, where ARM aligns them to 8.

## Talking to the real host

A scenario can bridge a channel to a UDP socket, a TCP connection, a
pseudo-terminal or a Linux TAP interface. Bridged scenarios run in real time,
and `costar test` skips them because host traffic isn't deterministic. See
[`examples/udp_bridge.toml`](examples/udp_bridge.toml):

```bash
build/costar run examples/udp_bridge.toml --images build/firmware --until-ms 2000
```

To type into a firmware UART, put it on your terminal with `--console`. It
runs in real time; your keys go to the UART as you type them, and its output
is printed as raw text. Ctrl-] ends the run:

```bash
build/costar run tests/cli/zephyr_shell.toml --images build/firmware --console zephyr.uart
```

```
uart:~$ kernel version
Zephyr version 4.5.0
```

- **Piped input.** It works too, e.g. `printf 'kernel version\n' | costar
  run ... --console ...`. When the input ends, the run ends once the firmware
  has been quiet for a second. A Zephyr shell drops what arrives before it
  starts, as a board does, so wait for it: `(sleep 1; printf ...) | ...`.
  If the program reading the output exits (`| head`), the run ends.
- **The trace.** It goes only to `--trace FILE`.

Other ways in:
- **A pty bridge** (`kind = "pty"`): `screen`, `picocom` or `minicom` attach
  to it, and can leave and come back. See
  [`examples/pty_console.toml`](examples/pty_console.toml).
- **A raw TCP bridge** (`framing = "raw"`): `nc` and `telnet` can connect.

Zephyr firmware can run its own network stack on a costar Ethernet device
(`costar,eth`), between machines or bridged to a TAP interface: the host can
ping it. See
[`examples/zephyr/tap_bridge.toml`](examples/zephyr/tap_bridge.toml).

## Remote API

`costar-grpc` serves interactive sessions over gRPC
([`proto/costar/v1/costar.proto`](proto/costar/v1/costar.proto)):
- create a session from a scenario, or clone one;
- run and pause it;
- send inputs;
- seek back and forth in time;
- inspect machines and devices;
- watch the trace live.

```bash
sudo apt-get install libgrpc++-dev libprotobuf-dev protobuf-compiler protobuf-compiler-grpc
cmake -S . -B build -DCOSTAR_BUILD_GRPC=ON && cmake --build build -j
build/costar-grpc --listen 127.0.0.1:9321
```

## Scripted tests in Python

[`python/`](python) is a client for the remote API and a pytest plugin, for
tests that talk to firmware as they go, as Renode's Robot tests do: send a
frame, wait for the answer, check a register. Everything happens in virtual
time, so a test gives the same result every run.

```bash
pip install ./python pytest     # needs grpcio-tools to build: the stubs are generated from the proto
pytest my_tests --costar-grpc build/costar-grpc --costar-images build/firmware
```

```python
from costar import ms

def test_echo(costar_session):
    session = costar_session("scenarios/uart_echo.toml")   # destroyed after the test
    session.wait_for("log", r"^boot \d+$", machine="zephyr", timeout=ms(10))
    session.write("zephyr.uart", "hi")                      # arrives now, as if typed
    line = session.wait_for("log", r"^got i at", timeout=ms(10))
    assert line.at < ms(5)
```

- `wait_for(label, text, machine=, value=, timeout=)` runs until a trace
  record matches (`text` is a regular expression) and returns it. The
  session stops at that record's instant, so what the test does next lands
  at the same point every run. `timeout` is virtual time.
- `run_until(t)`, `write`/`deliver`, `send`, `set_register`, `write_memory`,
  `power`, `inspect()` (devices, and each thread's stack use), `trace()`,
  `seek(t)` and `clone()` map onto the API.
- **Plants in Python:** `session.lockstep(period, until, step)` calls
  `step` every `period` of virtual time, with the devices as they are just
  before then; the inputs it makes take effect at that instant, as a
  compiled plant's would. They are recorded, so `seek` replays them.
  [`python/tests/test_python_plant.py`](python/tests/test_python_plant.py)
  closes the thermostat's loop and gets the compiled plant's exact trace.
- `costar.Server(path)` starts a server on a free port and shuts it down
  cleanly; `costar.Client(address)` connects to one that is running.

The fixtures are `costar_server`, `costar_images` and `costar_session`.
costar's own build runs [`python/tests`](python/tests) under ctest when it
builds `costar-grpc` and finds a Python with `grpcio`, `grpcio-tools` and
`pytest` (`-DCOSTAR_PYTHON=/path/to/venv/bin/python`).

## Running firmware from C++

```cpp
costar::World world;
world.add_machine("build/firmware/freertos_ping_pong_1.so");
world.add_machine("build/firmware/freertos_ping_pong_1.so");  // an independent second copy
world.run_until(costar::milliseconds(20));
std::cout << world.trace().format();
```

## Using costar in your project

Install costar, then use it as a CMake package. Installing needs
Boost.Context from the system (`libboost-context-dev`):

```bash
cmake --install build --prefix /opt/costar
```

```cmake
find_package(costar 0.1 REQUIRED)   # with CMAKE_PREFIX_PATH=/opt/costar
```

That gives:
- `costar::costar`, the engine;
- `costar::costar-cli`, the `costar` command;
- the CMake functions below.

[`examples/consumer`](examples/consumer) is a whole project: FreeRTOS
firmware, a `costar` command with a plant of its own, and its scenarios
registered as tests. costar's tests install costar and build it.
[`docker/Dockerfile`](docker/Dockerfile) builds a CI image with costar
installed and Zephyr ready.

Without installing, include the modules from a costar checkout instead:
`include(path/to/costar/cmake/CostarFirmware.cmake)`.

To build your own firmware as an image:

```cmake
costar_add_freertos_image(my_freertos_app
  SOURCES main.c
  CONFIG_DIR ${CMAKE_CURRENT_SOURCE_DIR}    # where your FreeRTOSConfig.h lives
  INSTRUMENT)                               # optional: code takes time (see below)

costar_add_zephyr_image(my_zephyr_app
  APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_app)   # an ordinary Zephyr application
```

By default firmware runs on an infinitely fast CPU: code takes no time.
With `INSTRUMENT` (both functions take it), each basic block of the
application takes time, 10 ns unless a scenario gives the machine another
speed (`cpu = { ns_per_block = 25 }`). Busy loops can then be preempted, and
slow code can miss its deadlines. `INSTRUMENT_KERNEL` charges the kernel's
work too. See
[ARCHITECTURE.md §2](docs/ARCHITECTURE.md#2-execution-model-an-infinitely-fast-cpu).
