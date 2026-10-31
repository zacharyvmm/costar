cmake_minimum_required(VERSION 3.28)
project(costar VERSION 0.1.0 LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_C_STANDARD 11)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

option(COSTAR_BUILD_TESTS "Build costar's tests and test firmware" ${PROJECT_IS_TOP_LEVEL})

include(FetchContent)
include(GNUInstallDirs)
include(cmake/CostarFirmware.cmake)
include(cmake/CostarScenarios.cmake)

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
  src/costar/host.cpp
  src/costar/image.cpp
  src/costar/machine.cpp
  src/costar/scenario.cpp
  src/costar/session.cpp
  src/costar/trace.cpp
  src/costar/wires.cpp
  src/costar/world.cpp)
target_include_directories(costar PUBLIC
  $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/src>
  $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
  $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>)
find_package(Threads REQUIRED)
target_link_libraries(costar PUBLIC Boost::context Threads::Threads ${CMAKE_DL_LIBS}
                             PRIVATE $<BUILD_INTERFACE:tomlplusplus::tomlplusplus>)
target_compile_options(costar PRIVATE -Wall -Wextra)
costar_instrument(costar)

# ── The costar command ────────────────────────────────────────────────

add_executable(costar-cli apps/costar/main.cpp apps/costar/thermal_plant.cpp)
target_link_libraries(costar-cli PRIVATE costar)
set_target_properties(costar-cli PROPERTIES OUTPUT_NAME costar)
costar_instrument(costar-cli)

# ── The gRPC server (optional) ────────────────────────────────────────

option(COSTAR_BUILD_GRPC "Build costar-grpc (needs gRPC and protobuf)" OFF)
if(COSTAR_BUILD_GRPC)
  find_package(gRPC CONFIG REQUIRED)
  set(proto_dir ${CMAKE_CURRENT_SOURCE_DIR}/proto)
  set(proto_out ${CMAKE_CURRENT_BINARY_DIR}/proto)
  set(proto_sources
    ${proto_out}/costar/v1/costar.pb.cc ${proto_out}/costar/v1/costar.pb.h
    ${proto_out}/costar/v1/costar.grpc.pb.cc ${proto_out}/costar/v1/costar.grpc.pb.h)
  add_custom_command(
    OUTPUT ${proto_sources}
    COMMAND ${CMAKE_COMMAND} -E make_directory ${proto_out}
    COMMAND protobuf::protoc --proto_path=${proto_dir} --cpp_out=${proto_out} --grpc_out=${proto_out}
            --plugin=protoc-gen-grpc=$<TARGET_FILE:gRPC::grpc_cpp_plugin>
            ${proto_dir}/costar/v1/costar.proto
    DEPENDS ${proto_dir}/costar/v1/costar.proto
    COMMENT "Generating the costar.v1 protobuf and gRPC code")

  add_library(costar_grpc src/costar_grpc/server.cpp ${proto_sources})
  target_include_directories(costar_grpc PUBLIC
    $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/src>
    $<BUILD_INTERFACE:${proto_out}>
    $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>)
  target_link_libraries(costar_grpc PUBLIC costar gRPC::grpc++ protobuf::libprotobuf)

  add_executable(costar-grpc apps/costar-grpc/main.cpp apps/costar/thermal_plant.cpp)
  target_link_libraries(costar-grpc PRIVATE costar_grpc)
  costar_instrument(costar_grpc)
  costar_instrument(costar-grpc)
endif()

# ── Installing ────────────────────────────────────────────────────────
#
# find_package(costar) then gives costar::costar (the engine), the costar
# command (costar::costar-cli), costar::costar_grpc and costar::costar-grpc
# if built with gRPC, and the CMake functions: costar_add_freertos_image(),
# costar_add_zephyr_image(), costar_add_scenario_tests(), costar_instrument().
# What the image functions build from (ports, costar.h, the FreeRTOS kernel)
# is installed in share/costar, in the same layout as this tree.

# The installed engine needs Boost.Context from the system; a fetched one
# can't be exported with it.
include(CMakeDependentOption)
cmake_dependent_option(COSTAR_INSTALL "Generate install rules" ON "Boost_FOUND" OFF)
if(COSTAR_INSTALL)
  include(CMakePackageConfigHelpers)
  set(data_dir ${CMAKE_INSTALL_DATADIR}/costar)
  set(config_dir ${CMAKE_INSTALL_LIBDIR}/cmake/costar)

  install(TARGETS costar costar-cli EXPORT costarTargets)
  install(FILES include/costar.h include/costar_host.h DESTINATION ${CMAKE_INSTALL_INCLUDEDIR})
  install(DIRECTORY src/costar DESTINATION ${CMAKE_INSTALL_INCLUDEDIR} FILES_MATCHING PATTERN "*.hpp")
  if(TARGET costar_grpc)
    install(TARGETS costar_grpc costar-grpc EXPORT costarTargets)
    install(DIRECTORY src/costar_grpc DESTINATION ${CMAKE_INSTALL_INCLUDEDIR} FILES_MATCHING PATTERN "*.hpp")
    install(DIRECTORY ${proto_out}/costar DESTINATION ${CMAKE_INSTALL_INCLUDEDIR} FILES_MATCHING PATTERN "*.h")
  endif()
  install(EXPORT costarTargets NAMESPACE costar:: DESTINATION ${config_dir})

  install(DIRECTORY cmake include ports DESTINATION ${data_dir})
  set(kernel ${COSTAR_FREERTOS_KERNEL_DIR})
  install(DIRECTORY ${kernel}/include DESTINATION ${data_dir}/third_party/FreeRTOS-Kernel)
  install(DIRECTORY ${kernel}/portable/MemMang DESTINATION ${data_dir}/third_party/FreeRTOS-Kernel/portable)
  install(FILES ${kernel}/tasks.c ${kernel}/queue.c ${kernel}/list.c ${kernel}/timers.c ${kernel}/event_groups.c
                ${kernel}/stream_buffer.c ${kernel}/LICENSE.md
          DESTINATION ${data_dir}/third_party/FreeRTOS-Kernel)

  set(COSTAR_WITH_GRPC ${COSTAR_BUILD_GRPC})
  configure_package_config_file(cmake/costarConfig.cmake.in ${CMAKE_CURRENT_BINARY_DIR}/costarConfig.cmake
    INSTALL_DESTINATION ${config_dir}
    PATH_VARS data_dir)
  write_basic_package_version_file(${CMAKE_CURRENT_BINARY_DIR}/costarConfigVersion.cmake
    COMPATIBILITY SameMinorVersion)
  install(FILES ${CMAKE_CURRENT_BINARY_DIR}/costarConfig.cmake ${CMAKE_CURRENT_BINARY_DIR}/costarConfigVersion.cmake
          DESTINATION ${config_dir})
endif()

# ── Tests ─────────────────────────────────────────────────────────────

if(COSTAR_BUILD_TESTS)
  enable_testing()
  add_subdirectory(firmware)
  add_subdirectory(tests)
endif()
