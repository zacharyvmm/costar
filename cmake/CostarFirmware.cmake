# Build firmware images that costar can load.
#
#   costar_add_freertos_image(<name>
#       SOURCES <app sources...>
#       CONFIG_DIR <directory containing FreeRTOSConfig.h>
#       [DEFINITIONS <compile definitions...>]
#       [HEAP heap_4]                 # heap_costar in AddressSanitizer builds
#       [INSTRUMENT])
#
# An image is a shared object containing the unmodified FreeRTOS kernel, the
# costar port, and the application.  It exports only main() and
# costar_image_attach(), so costar can load several independent copies.
#
# HEAP is one of FreeRTOS's heaps, or heap_costar
# (ports/freertos/heap_costar.c): a host malloc() per block, so that
# AddressSanitizer sees firmware's heap bugs.
#
# INSTRUMENT makes busy application code preemptible: the application's
# sources (not the kernel's) are compiled with -fsanitize-coverage=trace-pc,
# and ports/common/busy_poll.c charges virtual time for them and lets
# interrupts in (see that file).

set(COSTAR_ROOT ${CMAKE_CURRENT_LIST_DIR}/..)

# Sanitizer and coverage builds.  Every image built with the functions below
# gets them, and so does any target given to costar_instrument(): build the
# program that loads the images the same way (costar's own targets are).
set(COSTAR_SANITIZE "" CACHE STRING "Sanitizers for costar and its images, e.g. address,undefined")
option(COSTAR_COVERAGE "Build costar and its images with gcov coverage" OFF)

function(costar_instrument target)
  if(COSTAR_SANITIZE)
    # Every finding is fatal, so a test fails on it.
    target_compile_options(${target} PRIVATE -fsanitize=${COSTAR_SANITIZE} -fno-sanitize-recover=all
                                             -fno-omit-frame-pointer)
    target_link_options(${target} PRIVATE -fsanitize=${COSTAR_SANITIZE})
  endif()
  if(COSTAR_COVERAGE)
    target_compile_options(${target} PRIVATE --coverage)
    target_link_options(${target} PRIVATE --coverage)
  endif()
endfunction()

# Images are linked with --no-undefined, except where a sanitizer's runtime
# comes from the program (Clang links it into executables only).
set(costar_image_no_undefined -Wl,--no-undefined)
if(COSTAR_SANITIZE AND CMAKE_C_COMPILER_ID MATCHES "Clang")
  set(costar_image_no_undefined)
endif()
set(COSTAR_FREERTOS_KERNEL_DIR ${COSTAR_ROOT}/third_party/FreeRTOS-Kernel
    CACHE PATH "FreeRTOS-Kernel source tree")

function(costar_add_freertos_image name)
  cmake_parse_arguments(ARG "INSTRUMENT" "CONFIG_DIR;HEAP" "SOURCES;DEFINITIONS" ${ARGN})
  if(NOT ARG_HEAP AND COSTAR_SANITIZE MATCHES "address")
    set(ARG_HEAP heap_costar)  # ASan can't see inside heap_4's static array
  elseif(NOT ARG_HEAP)
    set(ARG_HEAP heap_4)
  endif()
  if(ARG_HEAP STREQUAL "heap_costar")
    set(heap ${COSTAR_ROOT}/ports/freertos/heap_costar.c)
  else()
    set(heap ${COSTAR_FREERTOS_KERNEL_DIR}/portable/MemMang/${ARG_HEAP}.c)
  endif()
  set(kernel ${COSTAR_FREERTOS_KERNEL_DIR})

  set(app_sources ${ARG_SOURCES})
  if(ARG_INSTRUMENT)
    # The application's sources, and only those, compile instrumented, and
    # only for this image (another image may share them).  Otherwise they
    # compile exactly as the image's other sources do.
    set_property(SOURCE ${ARG_SOURCES} APPEND PROPERTY COMPILE_OPTIONS
      "$<$<STREQUAL:$<TARGET_PROPERTY:NAME>,${name}>:-fsanitize-coverage=trace-pc>")
    list(APPEND app_sources ${COSTAR_ROOT}/ports/common/busy_poll.c)
  endif()

  add_library(${name} MODULE
    ${app_sources}
    ${kernel}/tasks.c
    ${kernel}/queue.c
    ${kernel}/list.c
    ${kernel}/timers.c
    ${kernel}/event_groups.c
    ${kernel}/stream_buffer.c
    ${heap}
    ${COSTAR_ROOT}/ports/freertos/port.c
    ${COSTAR_ROOT}/ports/common/image_stub.c)
  target_include_directories(${name} PRIVATE
    ${ARG_CONFIG_DIR}
    ${kernel}/include
    ${COSTAR_ROOT}/ports/freertos
    ${COSTAR_ROOT}/include)
  target_compile_definitions(${name} PRIVATE ${ARG_DEFINITIONS})
  target_link_options(${name} PRIVATE
    -Wl,--version-script=${COSTAR_ROOT}/ports/common/image.map
    ${costar_image_no_undefined})
  set_target_properties(${name} PROPERTIES
    PREFIX ""
    POSITION_INDEPENDENT_CODE ON
    LINK_DEPENDS ${COSTAR_ROOT}/ports/common/image.map)
  costar_instrument(${name})
endfunction()

#   costar_add_zephyr_image(<name>
#       APP_DIR <Zephyr application directory>
#       [BOARD native_sim/native/64]
#       [ZEPHYR_ARGS <extra -D... for the Zephyr build>])
#
# Builds the application with an ordinary Zephyr native_sim build, then links
# its embedded part (zephyr/zephyr.elf) with costar's native_sim runner
# instead of Zephyr's.  The build includes costar's Zephyr module
# (ports/zephyr/module), whose drivers put costar devices behind Zephyr's
# UART and flash APIs.  Needs COSTAR_ZEPHYR_BASE (default: $ENV{ZEPHYR_BASE}),
# a Python with Zephyr's requirements (COSTAR_ZEPHYR_PYTHON) and GNU Make
# (COSTAR_ZEPHYR_MAKE), which Zephyr's native_sim build needs anyway.

set(COSTAR_ZEPHYR_BASE "$ENV{ZEPHYR_BASE}" CACHE PATH "Zephyr source tree; enables Zephyr images")
find_program(COSTAR_ZEPHYR_PYTHON NAMES python3 python DOC "Python with Zephyr's requirements installed")
find_program(COSTAR_ZEPHYR_MAKE make DOC "GNU Make, which expands the native_sim runner's link inputs")
find_program(COSTAR_ZEPHYR_NINJA ninja DOC "Ninja for Zephyr builds (falls back to Make)")
find_program(COSTAR_WEST west DOC "west, to tell whether the Zephyr tree is in a west workspace")

function(costar_add_zephyr_image name)
  cmake_parse_arguments(ARG "" "APP_DIR;BOARD" "ZEPHYR_ARGS" ${ARGN})
  if(NOT COSTAR_ZEPHYR_BASE)
    message(FATAL_ERROR "costar_add_zephyr_image(${name}) needs COSTAR_ZEPHYR_BASE or ZEPHYR_BASE")
  endif()
  if(NOT ARG_BOARD)
    set(ARG_BOARD native_sim/native/64)
  endif()
  if(COSTAR_ZEPHYR_NINJA)
    set(generator Ninja -DCMAKE_MAKE_PROGRAM=${COSTAR_ZEPHYR_NINJA})
  else()
    set(generator "Unix Makefiles")
  endif()

  # costar's Zephyr module (drivers, and the `costar` shield that disables
  # native_sim's host-backed devices).  Without west, Zephyr only honours
  # ZEPHYR_MODULES; inside a west workspace that would replace west's list.
  set(module ${COSTAR_ROOT}/ports/zephyr/module)
  set(in_workspace 1)
  if(COSTAR_WEST)
    execute_process(COMMAND ${COSTAR_WEST} topdir WORKING_DIRECTORY ${COSTAR_ZEPHYR_BASE}
                    RESULT_VARIABLE in_workspace OUTPUT_QUIET ERROR_QUIET)
  endif()
  if(in_workspace EQUAL 0)
    set(module_arg -DZEPHYR_EXTRA_MODULES=${module})
  else()
    set(module_arg -DZEPHYR_MODULES=${module})
  endif()

  set(zephyr_build ${CMAKE_CURRENT_BINARY_DIR}/${name}-zephyr)
  set(embedded ${zephyr_build}/zephyr/zephyr.elf)
  set(link_options ${zephyr_build}/costar-link-options.rsp)
  set(link_libraries ${zephyr_build}/costar-link-libraries.rsp)
  set(zephyr_env ZEPHYR_BASE=${COSTAR_ZEPHYR_BASE} ZEPHYR_TOOLCHAIN_VARIANT=host)
  set(configure_args -G ${generator} -S ${ARG_APP_DIR} -B ${zephyr_build}
      -DBOARD=${ARG_BOARD} -DPython3_EXECUTABLE=${COSTAR_ZEPHYR_PYTHON}
      ${module_arg} -DSHIELD=costar ${ARG_ZEPHYR_ARGS})

  # Configure now, and again whenever these arguments change (Zephyr's own
  # build re-runs its configuration when Kconfig or devicetree inputs change,
  # but cannot know about arguments it is never given again).
  set(args_stamp ${zephyr_build}/costar-configure-args.txt)
  set(previous_args "")
  if(EXISTS ${args_stamp})
    file(READ ${args_stamp} previous_args)
  endif()
  if(NOT EXISTS ${zephyr_build}/CMakeCache.txt OR NOT "${previous_args}" STREQUAL "${configure_args}")
    message(STATUS "Configuring Zephyr image ${name} (${ARG_BOARD})")
    execute_process(
      COMMAND ${CMAKE_COMMAND} -E env ${zephyr_env} ${CMAKE_COMMAND} ${configure_args}
      RESULT_VARIABLE result OUTPUT_VARIABLE output ERROR_VARIABLE output)
    if(NOT result EQUAL 0)
      message(FATAL_ERROR "Zephyr configuration of ${name} failed:\n${output}")
    endif()
    file(WRITE ${args_stamp} "${configure_args}")
  endif()

  # Build the embedded image, then collect the link inputs Zephyr's own
  # runner would have used (e.g. -lstdc++ for C++ applications).
  add_custom_target(${name}_zephyr
    COMMAND ${CMAKE_COMMAND} -E env ${zephyr_env}
            ${CMAKE_COMMAND} -DBUILD=${zephyr_build} -P ${COSTAR_ROOT}/cmake/costar_build_zephyr_embedded.cmake
    COMMAND ${COSTAR_ZEPHYR_PYTHON} ${COSTAR_ROOT}/cmake/costar_nsi_link_options.py ${COSTAR_ZEPHYR_MAKE}
            ${zephyr_build}/zephyr/NSI/nsi_config ${link_options} ${link_libraries}
    BYPRODUCTS ${embedded} ${link_options} ${link_libraries}
    COMMENT "Building Zephyr application ${name}"
    USES_TERMINAL)

  # The runner: costar's, plus upstream's host trampolines (libc wrappers
  # that ztest and some drivers call).
  add_library(${name} MODULE
    ${embedded}
    ${COSTAR_ROOT}/ports/zephyr/runner.c
    ${COSTAR_ZEPHYR_BASE}/scripts/native_simulator/common/src/nsi_host_trampolines.c
    ${COSTAR_ROOT}/ports/common/image_stub.c)
  set_source_files_properties(${embedded} PROPERTIES EXTERNAL_OBJECT TRUE GENERATED TRUE)
  add_dependencies(${name} ${name}_zephyr)
  target_include_directories(${name} PRIVATE ${COSTAR_ROOT}/include)
  target_link_options(${name} PRIVATE
    -Wl,--version-script=${COSTAR_ROOT}/ports/common/image.map
    ${costar_image_no_undefined}
    @${link_options})
  target_link_libraries(${name} PRIVATE -Wl,@${link_libraries})
  set_target_properties(${name} PROPERTIES
    PREFIX ""
    POSITION_INDEPENDENT_CODE ON
    LINK_DEPENDS "${COSTAR_ROOT}/ports/common/image.map;${link_options};${link_libraries}")
  # Only costar's runner and stub: Zephyr builds the embedded image itself.
  costar_instrument(${name})
endfunction()
