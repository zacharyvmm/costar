# Test firmware: plain FreeRTOS and Zephyr applications.
set(freertos_config ${CMAKE_CURRENT_SOURCE_DIR}/freertos_config)

# The same application with two send periods: a 1-tick period exercises the
# idle-loop halt, a 3-tick period exercises tickless idle.
foreach(period 1 3)
  costar_add_freertos_image(freertos_ping_pong_${period}
    SOURCES freertos_ping_pong/main.c
    CONFIG_DIR ${freertos_config}
    DEFINITIONS SEND_PERIOD_TICKS=${period})
endforeach()

costar_add_freertos_image(freertos_link_node
  SOURCES freertos_link_node/main.c
  CONFIG_DIR ${freertos_config})

costar_add_freertos_image(freertos_thermostat
  SOURCES freertos_thermostat/main.c
  CONFIG_DIR ${freertos_config})

costar_add_freertos_image(freertos_errno
  SOURCES freertos_errno/main.c
  CONFIG_DIR ${freertos_config})

# Zephyr test firmware (needs a Zephyr tree; see cmake/CostarFirmware.cmake).
if(COSTAR_ZEPHYR_BASE)
  costar_add_zephyr_image(zephyr_ping_pong
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_ping_pong)
endif()

if(COSTAR_ZEPHYR_BASE)
  costar_add_zephyr_image(zephyr_uart_flash
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_uart_flash)
endif()

costar_add_freertos_image(freertos_kernel_probe
  SOURCES freertos_kernel_probe/main.c
  CONFIG_DIR ${freertos_config})

if(COSTAR_ZEPHYR_BASE)
  costar_add_zephyr_image(zephyr_ztest
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_ztest)
endif()

costar_add_freertos_image(freertos_busy_loop
  SOURCES freertos_busy_loop/main.c
  CONFIG_DIR ${freertos_config}
  INSTRUMENT)
