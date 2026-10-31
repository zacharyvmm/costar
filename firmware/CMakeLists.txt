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
  costar_add_zephyr_image(zephyr_entropy
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_entropy)
endif()

if(COSTAR_ZEPHYR_BASE)
  costar_add_zephyr_image(zephyr_gpio
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_gpio)
endif()

if(COSTAR_ZEPHYR_BASE)
  costar_add_zephyr_image(zephyr_can
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_can)
  costar_add_zephyr_image(zephyr_can_fd
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_can
    ZEPHYR_ARGS -DCONFIG_CAN_FD_MODE=y)
endif()

if(COSTAR_ZEPHYR_BASE)
  costar_add_zephyr_image(zephyr_i2c
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_i2c)
endif()

if(COSTAR_ZEPHYR_BASE)
  costar_add_zephyr_image(zephyr_spi
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_spi)
  costar_add_zephyr_image(zephyr_spi_nvs
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_spi
    ZEPHYR_ARGS -DCONFIG_NVS=y)
endif()

if(COSTAR_ZEPHYR_BASE)
  costar_add_zephyr_image(zephyr_ztest
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_ztest)
  costar_add_zephyr_image(zephyr_stack_overflow
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_stack_overflow)
  # A busy loop that a timer interrupt ends: the application instrumented, or
  # the kernel too.
  costar_add_zephyr_image(zephyr_busy_loop
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_busy_loop
    INSTRUMENT)
  costar_add_zephyr_image(zephyr_busy_loop_kernel
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_busy_loop
    INSTRUMENT_KERNEL)
  costar_add_zephyr_image(zephyr_watchdog
    APP_DIR ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_watchdog)
endif()

costar_add_freertos_image(freertos_busy_loop
  SOURCES freertos_busy_loop/main.c
  CONFIG_DIR ${freertos_config}
  INSTRUMENT)

# CPU time: a control task's deadlines, met or missed depending on the
# machine's speed.  The kernel's work costs time too; time is charged every
# 10 blocks, finely enough to see it (the tests compare it with
# freertos_deadline_app, where only the application is instrumented).
foreach(variant "" _app)
  if(variant STREQUAL "")
    set(instrument INSTRUMENT_KERNEL)
  else()
    set(instrument INSTRUMENT)
  endif()
  costar_add_freertos_image(freertos_deadline${variant}
    SOURCES freertos_deadline/main.c
    CONFIG_DIR ${freertos_config}
    DEFINITIONS INCLUDE_xTaskDelayUntil=1 COSTAR_POLL_BLOCKS=10
    ${instrument})
endforeach()

# The same busy loop without INSTRUMENT: it never ends (the CLI's timeout test).
costar_add_freertos_image(freertos_busy_loop_plain
  SOURCES freertos_busy_loop/main.c
  CONFIG_DIR ${freertos_config})

costar_add_freertos_image(freertos_memory_bug
  SOURCES freertos_memory_bug/main.c
  CONFIG_DIR ${freertos_config})

costar_add_freertos_image(freertos_stall
  SOURCES freertos_stall/main.c
  CONFIG_DIR ${freertos_config})

costar_add_freertos_image(freertos_stack_overflow
  SOURCES freertos_stack_overflow/main.c
  CONFIG_DIR ${freertos_config})

costar_add_freertos_image(freertos_watchdog
  SOURCES freertos_watchdog/main.c
  CONFIG_DIR ${freertos_config})
