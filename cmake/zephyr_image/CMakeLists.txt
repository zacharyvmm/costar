# Links a built Zephyr application's costar image, <ZEPHYR_BUILD>/zephyr/zephyr.so,
# as costar_add_zephyr_image() links its images.  The native_sim/native/64/costar
# board (ports/zephyr/module/costar_zephyr_exe.cmake) builds this project from
# inside the application's Zephyr build, whose own CMake project can't build
# shared objects.
#   cmake -S <this directory> -B <dir> -DZEPHYR_BUILD=<dir> -DZEPHYR_BASE=<dir>
#         [-DCOSTAR_INSTRUMENT=app|kernel]   # the Zephyr build's: link busy_poll.c
cmake_minimum_required(VERSION 3.20)
project(costar_zephyr_image C)

include(${CMAKE_CURRENT_LIST_DIR}/../CostarFirmware.cmake)

# The link inputs Zephyr's own runner would have used.
set(rsp ${ZEPHYR_BUILD}/costar-link-options.rsp ${ZEPHYR_BUILD}/costar-link-libraries.rsp)
add_custom_command(
  OUTPUT ${rsp}
  COMMAND ${COSTAR_ZEPHYR_PYTHON} ${COSTAR_ROOT}/cmake/costar_nsi_link_options.py ${COSTAR_ZEPHYR_MAKE}
          ${ZEPHYR_BUILD}/zephyr/NSI/nsi_config ${rsp}
  DEPENDS ${ZEPHYR_BUILD}/zephyr/NSI/nsi_config ${COSTAR_ROOT}/cmake/costar_nsi_link_options.py)
add_custom_target(link_inputs DEPENDS ${rsp})

# An instrumented application (COSTAR_INSTRUMENT, as costar_add_zephyr_image()'s
# INSTRUMENT and INSTRUMENT_KERNEL) calls busy_poll.c's __sanitizer_cov_trace_pc.
set(runner_sources "")
if(COSTAR_INSTRUMENT)
  set(runner_sources ${COSTAR_ROOT}/ports/common/busy_poll.c)
endif()
costar_link_zephyr_image(zephyr ${ZEPHYR_BUILD} ${ZEPHYR_BASE} SOURCES ${runner_sources})
add_dependencies(zephyr link_inputs)
set_target_properties(zephyr PROPERTIES LIBRARY_OUTPUT_DIRECTORY ${ZEPHYR_BUILD}/zephyr)
