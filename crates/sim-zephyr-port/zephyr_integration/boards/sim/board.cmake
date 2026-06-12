# board.cmake — CMake configuration for the sim board
#
# Place at: boards/sim/board.cmake
#
# Tells Zephyr's build system where to find the board's source files
# and what compiler flags to use.

# ── Board source files ─────────────────────────────────────────────────
# The arch port files (sim_arch.c etc.) are provided by ZEPHYR_BASE/arch/sim/.
# No board-specific C files are needed — the Rust bridge handles everything.

# ── Compiler flags ────────────────────────────────────────────────────
# Link with the simulator's ABI.  The sim_abi.h and sim_zephyr_abi.h
# headers must be in the include path.

set(BOARD_CPPFLAGS
  -DSIMULATION_HOST_MODE=1
  -DZEPHYR_PORT_SIM=1
  -include sim_abi.h
  )

# ── Linker ────────────────────────────────────────────────────────────
# Use a flat linker script — the simulator process has the full host
# address space.  No MCU memory layout needed.

set(BOARD_LINKER_SCRIPT
  ${ZEPHYR_BASE}/include/arch/sim/linker.ld
  )

# ── Build as static library ───────────────────────────────────────────
# The simulator (sim-runner) links against libzephyr.a.
# CARGO_BUILD_DIR can be set to the sim-runner's target directory,
# or the .a is copied manually after west build.

set(BOARD_BUILD_STATIC_LIBRARY y)

# ── Include paths for sim ABI headers ─────────────────────────────────
# Point to the sim-ffi and sim-zephyr-port include directories.
# These must be available when building the Zephyr kernel.

include_directories(
  ${CMAKE_SOURCE_DIR}/../crates/sim-ffi/include
  ${CMAKE_SOURCE_DIR}/../crates/sim-zephyr-port/c
  )
