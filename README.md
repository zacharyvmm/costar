# costar

Deterministic, virtual-time simulation of unmodified RTOS firmware on a single
host thread. Many machines can run side by side.

costar presents a small virtual CPU ([`include/costar.h`](include/costar.h)).
RTOSes run on it through their normal port contracts:
- **FreeRTOS:** an ordinary port in [`ports/freertos`](ports/freertos). No
  kernel patches.
- **Zephyr:** `native_sim` builds, which are planned.

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

## Running firmware

```cpp
costar::World world;
world.add_machine("build/firmware/freertos_ping_pong_1.so");
world.add_machine("build/firmware/freertos_ping_pong_1.so");  // an independent second copy
world.run_until(costar::milliseconds(20));
std::cout << world.trace().format();
```

To build your own FreeRTOS firmware as an image:

```cmake
include(path/to/costar/cmake/CostarFirmware.cmake)
costar_add_freertos_image(my_firmware
  SOURCES main.c
  CONFIG_DIR ${CMAKE_CURRENT_SOURCE_DIR})   # where your FreeRTOSConfig.h lives
```
