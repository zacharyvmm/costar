# Build firmware images that costar can load.
#
#   costar_add_freertos_image(<name>
#       SOURCES <app sources...>
#       CONFIG_DIR <directory containing FreeRTOSConfig.h>
#       [DEFINITIONS <compile definitions...>]
#       [HEAP heap_4])
#
# An image is a shared object containing the unmodified FreeRTOS kernel, the
# costar port, and the application.  It exports only main() and
# costar_image_attach(), so costar can load several independent copies.

set(COSTAR_ROOT ${CMAKE_CURRENT_LIST_DIR}/..)
set(COSTAR_FREERTOS_KERNEL_DIR ${COSTAR_ROOT}/third_party/FreeRTOS-Kernel
    CACHE PATH "FreeRTOS-Kernel source tree")

function(costar_add_freertos_image name)
  cmake_parse_arguments(ARG "" "CONFIG_DIR;HEAP" "SOURCES;DEFINITIONS" ${ARGN})
  if(NOT ARG_HEAP)
    set(ARG_HEAP heap_4)
  endif()
  set(kernel ${COSTAR_FREERTOS_KERNEL_DIR})

  add_library(${name} MODULE
    ${ARG_SOURCES}
    ${kernel}/tasks.c
    ${kernel}/queue.c
    ${kernel}/list.c
    ${kernel}/timers.c
    ${kernel}/event_groups.c
    ${kernel}/stream_buffer.c
    ${kernel}/portable/MemMang/${ARG_HEAP}.c
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
    -Wl,--no-undefined)
  set_target_properties(${name} PROPERTIES
    PREFIX ""
    POSITION_INDEPENDENT_CODE ON
    LINK_DEPENDS ${COSTAR_ROOT}/ports/common/image.map)
endfunction()

#   costar_add_zephyr_image(<name>
#       APP_DIR <Zephyr application directory>
#       [BOARD native_sim/native/64]
#       [ZEPHYR_ARGS <extra -D... for the Zephyr build>])
#
# Builds the application with an ordinary Zephyr native_sim build, then links
# its embedded part (zephyr/zephyr.elf) with costar's native_sim runner
# instead of Zephyr's.  Needs COSTAR_ZEPHYR_BASE (default: $ENV{ZEPHYR_BASE})
# a Python with Zephyr's requirements (COSTAR_ZEPHYR_PYTHON) and GNU Make
# (COSTAR_ZEPHYR_MAKE), which Zephyr's native_sim build needs anyway.

set(COSTAR_ZEPHYR_BASE "$ENV{ZEPHYR_BASE}" CACHE PATH "Zephyr source tree; enables Zephyr images")
find_program(COSTAR_ZEPHYR_PYTHON NAMES python3 python DOC "Python with Zephyr's requirements installed")
find_program(COSTAR_ZEPHYR_MAKE make DOC "GNU Make, which expands the native_sim runner's link inputs")
find_program(COSTAR_ZEPHYR_NINJA ninja DOC "Ninja for Zephyr builds (falls back to Make)")

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

  set(zephyr_build ${CMAKE_CURRENT_BINARY_DIR}/${name}-zephyr)
  set(embedded ${zephyr_build}/zephyr/zephyr.elf)
  set(link_options ${zephyr_build}/costar-link-options.rsp)
  set(link_libraries ${zephyr_build}/costar-link-libraries.rsp)
  set(zephyr_env ZEPHYR_BASE=${COSTAR_ZEPHYR_BASE} ZEPHYR_TOOLCHAIN_VARIANT=host)
  set(configure_args -G ${generator} -S ${ARG_APP_DIR} -B ${zephyr_build}
      -DBOARD=${ARG_BOARD} -DPython3_EXECUTABLE=${COSTAR_ZEPHYR_PYTHON} ${ARG_ZEPHYR_ARGS})

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
    COMMAND ${CMAKE_COMMAND} -E env ${zephyr_env} ${CMAKE_COMMAND} --build ${zephyr_build}
    COMMAND ${COSTAR_ZEPHYR_PYTHON} ${COSTAR_ROOT}/cmake/costar_nsi_link_options.py ${COSTAR_ZEPHYR_MAKE}
            ${zephyr_build}/zephyr/NSI/nsi_config ${link_options} ${link_libraries}
    BYPRODUCTS ${embedded} ${link_options} ${link_libraries}
    COMMENT "Building Zephyr application ${name}"
    USES_TERMINAL)

  add_library(${name} MODULE
    ${embedded}
    ${COSTAR_ROOT}/ports/zephyr/runner.c
    ${COSTAR_ROOT}/ports/common/image_stub.c)
  set_source_files_properties(${embedded} PROPERTIES EXTERNAL_OBJECT TRUE GENERATED TRUE)
  add_dependencies(${name} ${name}_zephyr)
  target_include_directories(${name} PRIVATE ${COSTAR_ROOT}/include)
  target_link_options(${name} PRIVATE
    -Wl,--version-script=${COSTAR_ROOT}/ports/common/image.map
    -Wl,--no-undefined
    @${link_options})
  target_link_libraries(${name} PRIVATE -Wl,@${link_libraries})
  set_target_properties(${name} PROPERTIES
    PREFIX ""
    POSITION_INDEPENDENT_CODE ON
    LINK_DEPENDS "${COSTAR_ROOT}/ports/common/image.map;${link_options};${link_libraries}")
endfunction()
