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

Besides a golden trace, a scenario can state what must (`[[expect.event]]`)
or must not (`[[expect.no]]`) happen, and when. Inputs can switch machines
off and on, and wire faults can drop, corrupt or delay matching frames. See
[`tests/scenarios/reboot.toml`](tests/scenarios/reboot.toml).

## Talking to the real host

A scenario can bridge a channel to a UDP socket, a TCP connection or a Linux
TAP interface. Bridged scenarios run in real time, and `costar test` skips
them because host traffic isn't deterministic. See
[`examples/udp_bridge.toml`](examples/udp_bridge.toml):

```bash
build/costar run examples/udp_bridge.toml --images build/firmware --until-ms 2000
```

## Remote API

`costar-grpc` serves interactive sessions over gRPC
([`proto/costar/v1/costar.proto`](proto/costar/v1/costar.proto)):
- create a session from a scenario;
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

## Running firmware from C++

```cpp
costar::World world;
world.add_machine("build/firmware/freertos_ping_pong_1.so");
world.add_machine("build/firmware/freertos_ping_pong_1.so");  // an independent second copy
world.run_until(costar::milliseconds(20));
std::cout << world.trace().format();
```

To build your own firmware as an image:

```cmake
include(path/to/costar/cmake/CostarFirmware.cmake)

costar_add_freertos_image(my_freertos_app
  SOURCES main.c
  CONFIG_DIR ${CMAKE_CURRENT_SOURCE_DIR}    # where your FreeRTOSConfig.h lives
  INSTRUMENT)                               # optional: busy loops can be preempted

costar_add_zephyr_image(my_zephyr_app
  APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_app)   # an ordinary Zephyr application
```
