# Zephyr Integration — Real Zephyr Build

This directory contains reference files for building a real Zephyr kernel
with the Universal RTOS Native Simulator as the target board.

## What's Here

```
zephyr_integration/
├── README.md
├── arch/
│   └── sim/
│       ├── Kconfig           # Architecture Kconfig (ARCH_SIM)
│       └── linker.ld         # Flat linker script (host address space)
└── boards/
    └── sim/
        ├── Kconfig           # Board selection option
        ├── Kconfig.defconfig # Board default config
        ├── sim.dts           # Minimal devicetree (UART, timer, intc)
        ├── sim_defconfig     # Default Kconfig values
        └── board.cmake       # CMake build config
```

## How to Use (External Zephyr Build)

### Prerequisites

1. Zephyr SDK installed (https://docs.zephyrproject.org/latest/develop/getting_started/index.html)
2. `west` CLI available
3. This repository cloned at `../universal-rtos-native-simulator/` relative to your Zephyr workspace

### Step 1: Copy Integration Files

Copy the arch and board files into your Zephyr source tree:

```bash
ZEPHYR_BASE=$(west topdir)/zephyr

# Arch port
cp -r arch/sim/ $ZEPHYR_BASE/arch/

# Board definition
cp -r boards/sim/ $ZEPHYR_BASE/boards/

# Arch port C source (the actual sim_arch.c / sim_arch.h)
cp ../../crates/sim-zephyr-port/c/zephyr_arch.c $ZEPHYR_BASE/arch/sim/core/
cp ../../crates/sim-zephyr-port/c/zephyr_arch.h $ZEPHYR_BASE/arch/sim/include/
cp ../../crates/sim-zephyr-port/c/sim_zephyr_abi.h $ZEPHYR_BASE/arch/sim/include/
cp ../../crates/sim-ffi/include/sim_abi.h $ZEPHYR_BASE/arch/sim/include/
```

### Step 2: Build Zephyr as a Static Library

```bash
cd your_zephyr_app/
west build -b sim -- -DCONFIG_BUILD_OUTPUT_STATIC_LIBRARY=y
```

This produces:
- `build/zephyr/libzephyr.a` — the compiled kernel + app
- `build/zephyr/include/generated/` — generated headers (autoconf.h, devicetree_generated.h)

### Step 3: Link into the Simulator

```bash
cd universal-rtos-native-simulator/
ZEPHYR_BUILD_DIR=../your_zephyr_app/build cargo run -- --rtos zephyr
```

(Full cargo integration for linking libzephyr.a is pending — see
Phase 14 in IMPLEMENTATION_STATUS.md.)

## Standalone Test (No Zephyr SDK Required)

For CI and quick verification, use the standalone test that compiles
directly through the `cc` crate:

```bash
cargo run -- --rtos zephyr
```

This runs `c_firmware/zephyr_app/standalone_test.c`, which demonstrates
the thread→fiber pattern without needing `west` or the Zephyr SDK.
