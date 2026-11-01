# Zephyr drivers for costar devices.  costar_add_zephyr_image() adds this
# module to every Zephyr image; the costar.h calls resolve to the image stub.
if(CONFIG_COSTAR_UART OR CONFIG_COSTAR_FLASH OR CONFIG_COSTAR_ENTROPY OR CONFIG_COSTAR_GPIO OR CONFIG_COSTAR_CAN
   OR CONFIG_COSTAR_WDT OR CONFIG_COSTAR_THREAD_INFO OR CONFIG_COSTAR_I2C OR CONFIG_COSTAR_SPI
   OR CONFIG_COSTAR_ADC OR CONFIG_COSTAR_PWM OR CONFIG_COSTAR_EEPROM OR CONFIG_COSTAR_ETH)
  zephyr_library_named(costar_drivers)
  zephyr_library_include_directories(${CMAKE_CURRENT_LIST_DIR}/../../../include)
  if(CONFIG_COSTAR_THREAD_INFO)
    zephyr_library_sources(costar_thread.c)
    # posix_core.h: the posix arch's per-thread state
    zephyr_library_include_directories(${ZEPHYR_BASE}/arch/posix/include)
  endif()
  zephyr_library_sources_ifdef(CONFIG_COSTAR_UART drivers/costar_uart.c)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_FLASH drivers/costar_flash.c)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_ENTROPY drivers/costar_entropy.c)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_GPIO drivers/costar_gpio.c)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_CAN drivers/costar_can.c)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_WDT drivers/costar_wdt.c)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_I2C drivers/costar_i2c.c)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_SPI drivers/costar_spi.c)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_ADC drivers/costar_adc.c)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_PWM drivers/costar_pwm.c)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_EEPROM drivers/costar_eeprom.c)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_ETH drivers/costar_eth.c)
  # The ADC uses Zephyr's own sequencing helper (drivers/adc/adc_context.h).
  if(CONFIG_COSTAR_ADC)
    zephyr_library_include_directories(${ZEPHYR_BASE}/drivers/adc)
  endif()
endif()

# costar_add_zephyr_image(... INSTRUMENT | INSTRUMENT_KERNEL): basic blocks of
# the application, and of the kernel, cost time (ports/common/busy_poll.c).
# The kernel library is defined after modules, so its flag is added once the
# Zephyr build's own directory is done.
if(COSTAR_INSTRUMENT STREQUAL "app" OR COSTAR_INSTRUMENT STREQUAL "kernel")
  target_compile_options(app PRIVATE -fsanitize-coverage=trace-pc)
  if(COSTAR_INSTRUMENT STREQUAL "kernel")
    cmake_language(DEFER DIRECTORY ${ZEPHYR_BASE} CALL target_compile_options kernel PRIVATE -fsanitize-coverage=trace-pc)
  endif()
elseif(COSTAR_INSTRUMENT)
  message(FATAL_ERROR "COSTAR_INSTRUMENT must be app or kernel, not '${COSTAR_INSTRUMENT}'")
endif()

# The native_sim/native/64/costar board (boards/costar): zephyr/zephyr.exe
# runs the application on costar, so what runs zephyr.exe (twister's native
# handler, say) runs it there.
if(CONFIG_BOARD_NATIVE_SIM_NATIVE_64_COSTAR)
  include(${CMAKE_CURRENT_LIST_DIR}/costar_zephyr_exe.cmake)
endif()
