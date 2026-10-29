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
  # Unwind tables let costar destroy a suspended task's stack cleanly.
  target_compile_options(${name} PRIVATE -funwind-tables)
  target_link_options(${name} PRIVATE
    -Wl,--version-script=${COSTAR_ROOT}/ports/common/image.map
    -Wl,--no-undefined)
  set_target_properties(${name} PROPERTIES
    PREFIX ""
    POSITION_INDEPENDENT_CODE ON
    LINK_DEPENDS ${COSTAR_ROOT}/ports/common/image.map)
endfunction()
