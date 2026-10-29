# Test firmware.  The same application is built with two send periods: a
# 1-tick period exercises the idle-loop halt, a 3-tick period exercises
# tickless idle.
foreach(period 1 3)
  costar_add_freertos_image(freertos_ping_pong_${period}
    SOURCES freertos_ping_pong/main.c
    CONFIG_DIR ${CMAKE_CURRENT_SOURCE_DIR}/freertos_ping_pong
    DEFINITIONS SEND_PERIOD_TICKS=${period})
endforeach()

# Zephyr test firmware (needs a Zephyr tree; see cmake/CostarFirmware.cmake).
if(COSTAR_ZEPHYR_BASE)
  costar_add_zephyr_image(zephyr_ping_pong
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_ping_pong)
endif()
