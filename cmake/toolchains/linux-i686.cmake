# Build costar, its tests and its firmware images as 32-bit x86 code on an
# x86_64 Linux host (needs gcc-multilib and g++-multilib, or Clang with the
# same 32-bit libraries).
#
#   cmake -S . -B build32 -DCMAKE_TOOLCHAIN_FILE=cmake/toolchains/linux-i686.cmake
#
# Firmware then runs with an ILP32 data model, as on Cortex-M and most other
# MCUs: 32-bit pointers, long and size_t.  Images are loaded into the engine,
# so the engine is 32-bit too.  Zephyr images are built for board native_sim
# (32-bit) instead of native_sim/native/64; see costar_add_zephyr_image().
set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR i686)

# Compiling and linking, C, C++ and assembly (Boost.Context's, when fetched).
set(CMAKE_C_FLAGS_INIT -m32)
set(CMAKE_CXX_FLAGS_INIT -m32)
set(CMAKE_ASM_FLAGS_INIT -m32)
