# Zephyr drivers for costar devices.  costar_add_zephyr_image() adds this
# module to every Zephyr image; the costar.h calls resolve to the image stub.
if(CONFIG_COSTAR_UART OR CONFIG_COSTAR_FLASH OR CONFIG_COSTAR_ENTROPY OR CONFIG_COSTAR_GPIO)
  zephyr_library_named(costar_drivers)
  zephyr_library_include_directories(${CMAKE_CURRENT_LIST_DIR}/../../../include)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_UART drivers/costar_uart.c)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_FLASH drivers/costar_flash.c)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_ENTROPY drivers/costar_entropy.c)
  zephyr_library_sources_ifdef(CONFIG_COSTAR_GPIO drivers/costar_gpio.c)
endif()
