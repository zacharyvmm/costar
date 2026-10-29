# Build only the embedded part of a Zephyr native_sim build (zephyr/zephyr.elf).
# Zephyr's own runner executable can't link an image that uses costar's
# drivers, and costar doesn't use it.  The embedded image is the last
# "pre" link stage: zephyr_pre1 with CONFIG_USERSPACE or CONFIG_DEVICE_DEPS,
# otherwise zephyr_pre0.  zephyr_pre0 is built first, which also brings
# Zephyr's configuration up to date before .config is read.
#   cmake -DBUILD=<zephyr build dir> -P costar_build_zephyr_embedded.cmake

function(build target)
  execute_process(COMMAND ${CMAKE_COMMAND} --build ${BUILD} --target ${target} RESULT_VARIABLE result)
  if(NOT result EQUAL 0)
    message(FATAL_ERROR "building ${target} in ${BUILD} failed")
  endif()
endfunction()

build(zephyr_pre0)
file(STRINGS ${BUILD}/zephyr/.config three_stage REGEX "^CONFIG_(USERSPACE|DEVICE_DEPS)=y$")
if(three_stage)
  build(zephyr_pre1)
endif()
