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
# and a Python with Zephyr's requirements (COSTAR_ZEPHYR_PYTHON).

set(COSTAR_ZEPHYR_BASE "$ENV{ZEPHYR_BASE}" CACHE PATH "Zephyr source tree; enables Zephyr images")
find_program(COSTAR_ZEPHYR_PYTHON NAMES python3 python DOC "Python with Zephyr's requirements installed")
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
  set(zephyr_env ZEPHYR_BASE=${COSTAR_ZEPHYR_BASE} ZEPHYR_TOOLCHAIN_VARIANT=host)

  # Configure once now; Zephyr's own build re-runs its configuration when
  # Kconfig or devicetree inputs change.
  if(NOT EXISTS ${zephyr_build}/CMakeCache.txt)
    message(STATUS "Configuring Zephyr image ${name} (${ARG_BOARD})")
    execute_process(
      COMMAND ${CMAKE_COMMAND} -E env ${zephyr_env}
              ${CMAKE_COMMAND} -G ${generator} -S ${ARG_APP_DIR} -B ${zephyr_build}
              -DBOARD=${ARG_BOARD} -DPython3_EXECUTABLE=${COSTAR_ZEPHYR_PYTHON} ${ARG_ZEPHYR_ARGS}
      RESULT_VARIABLE result OUTPUT_VARIABLE output ERROR_VARIABLE output)
    if(NOT result EQUAL 0)
      message(FATAL_ERROR "Zephyr configuration of ${name} failed:\n${output}")
    endif()
  endif()

  add_custom_target(${name}_zephyr
    COMMAND ${CMAKE_COMMAND} -E env ${zephyr_env} ${CMAKE_COMMAND} --build ${zephyr_build}
    BYPRODUCTS ${embedded}
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
    -Wl,--no-undefined)
  set_target_properties(${name} PROPERTIES
    PREFIX ""
    POSITION_INDEPENDENT_CODE ON
    LINK_DEPENDS ${COSTAR_ROOT}/ports/common/image.map)
endfunction()
