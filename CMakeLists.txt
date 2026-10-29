cmake_minimum_required(VERSION 3.28)
project(costar LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_C_STANDARD 11)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

option(COSTAR_BUILD_TESTS "Build costar's tests and test firmware" ${PROJECT_IS_TOP_LEVEL})

include(FetchContent)
include(cmake/CostarFirmware.cmake)

# ── Dependencies ──────────────────────────────────────────────────────

find_package(Boost 1.83 CONFIG QUIET COMPONENTS context)
if(NOT Boost_FOUND)
  message(STATUS "Boost.Context not found; fetching Boost 1.90 (builds only Boost.Context)")
  set(BOOST_INCLUDE_LIBRARIES context)
  FetchContent_Declare(Boost
    URL https://github.com/boostorg/boost/releases/download/boost-1.90.0/boost-1.90.0-cmake.tar.xz
    URL_HASH SHA256=aca59f889f0f32028ad88ba6764582b63c916ce5f77b31289ad19421a96c555f
    DOWNLOAD_EXTRACT_TIMESTAMP ON
    EXCLUDE_FROM_ALL)
  FetchContent_MakeAvailable(Boost)
endif()

FetchContent_Declare(tomlplusplus
  URL https://github.com/marzer/tomlplusplus/archive/refs/tags/v3.4.0.tar.gz
  URL_HASH SHA256=8517f65938a4faae9ccf8ebb36631a38c1cadfb5efa85d9a72e15b9e97d25155
  DOWNLOAD_EXTRACT_TIMESTAMP ON
  EXCLUDE_FROM_ALL)
FetchContent_MakeAvailable(tomlplusplus)

# ── Engine ────────────────────────────────────────────────────────────

add_library(costar
  src/costar/abi.cpp
  src/costar/cli.cpp
  src/costar/cpu.cpp
  src/costar/devices.cpp
  src/costar/events.cpp
  src/costar/image.cpp
  src/costar/machine.cpp
  src/costar/scenario.cpp
  src/costar/trace.cpp
  src/costar/wires.cpp
  src/costar/world.cpp)
target_include_directories(costar PUBLIC src include)
target_link_libraries(costar PUBLIC Boost::context ${CMAKE_DL_LIBS} PRIVATE tomlplusplus::tomlplusplus)
target_compile_options(costar PRIVATE -Wall -Wextra)

# ── The costar command ────────────────────────────────────────────────

add_executable(costar-cli apps/costar/main.cpp apps/costar/thermal_plant.cpp)
target_link_libraries(costar-cli PRIVATE costar)
set_target_properties(costar-cli PROPERTIES OUTPUT_NAME costar)

# ── Tests ─────────────────────────────────────────────────────────────

if(COSTAR_BUILD_TESTS)
  enable_testing()
  add_subdirectory(firmware)
  add_subdirectory(tests)
endif()
