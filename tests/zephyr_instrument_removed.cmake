# An image whose INSTRUMENT option is removed must reconfigure its Zephyr
# build without instrumentation.  That build keeps COSTAR_INSTRUMENT in its
# cache, and if it stayed `app`, the application would still be compiled with
# calls to __sanitizer_cov_trace_pc while the image no longer links
# busy_poll.c.  Configures a project with one Zephyr image, with INSTRUMENT
# and then without, in the same build directory, and checks the Zephyr
# build's cache each time.
#   cmake -DSOURCE=<costar> -DDIR=<scratch> -DGENERATOR=<...> [-DMAKE_PROGRAM=<...>]
#         -DCOMPILER=<C compiler> -DZEPHYR_BASE=<...> -DPYTHON=<Python for Zephyr>
#         -P zephyr_instrument_removed.cmake
file(REMOVE_RECURSE ${DIR})
file(WRITE ${DIR}/project/CMakeLists.txt "
cmake_minimum_required(VERSION 3.20)
project(zephyr_instrument_removed C)
include(${SOURCE}/cmake/CostarFirmware.cmake)
costar_add_zephyr_image(busy_loop APP_DIR ${SOURCE}/firmware/zephyr_busy_loop \${INSTRUMENT})
")

function(configure instrument expected)
  execute_process(
    COMMAND ${CMAKE_COMMAND} -S ${DIR}/project -B ${DIR}/build -G ${GENERATOR} -DCMAKE_MAKE_PROGRAM=${MAKE_PROGRAM}
            -DCMAKE_C_COMPILER=${COMPILER} -DCOSTAR_ZEPHYR_BASE=${ZEPHYR_BASE} -DCOSTAR_ZEPHYR_PYTHON=${PYTHON}
            -DINSTRUMENT=${instrument}
    RESULT_VARIABLE result OUTPUT_VARIABLE out ERROR_VARIABLE out)
  if(NOT result EQUAL 0)
    message(FATAL_ERROR "configuring with INSTRUMENT='${instrument}' failed:\n${out}")
  endif()
  file(STRINGS ${DIR}/build/busy_loop-zephyr/CMakeCache.txt cached REGEX "^COSTAR_INSTRUMENT:")
  string(REGEX REPLACE "^[^=]*=" "" cached "${cached}")
  if(NOT cached STREQUAL expected)
    message(FATAL_ERROR "with INSTRUMENT='${instrument}', the Zephyr build has COSTAR_INSTRUMENT='${cached}', "
                        "not '${expected}'")
  endif()
endfunction()

configure(INSTRUMENT app)
configure("" "")
file(REMOVE_RECURSE ${DIR})
