FetchContent_Declare(doctest
  URL https://github.com/doctest/doctest/archive/refs/tags/v2.5.3.tar.gz
  URL_HASH SHA256=174ebc4e769928959614789c5b4e9c3d0a0f81a62bb608756b127bfebfb21331
  DOWNLOAD_EXTRACT_TIMESTAMP ON
  EXCLUDE_FROM_ALL)
FetchContent_MakeAvailable(doctest)

add_executable(costar_tests main.cpp freertos_test.cpp devices_test.cpp scenario_test.cpp wires_test.cpp)
target_link_libraries(costar_tests PRIVATE costar doctest::doctest)
target_compile_definitions(costar_tests PRIVATE
  PING_PONG_1="$<TARGET_FILE:freertos_ping_pong_1>"
  PING_PONG_3="$<TARGET_FILE:freertos_ping_pong_3>"
  ERRNO_IMAGE="$<TARGET_FILE:freertos_errno>"
  LINK_NODE="$<TARGET_FILE:freertos_link_node>"
  THERMOSTAT="$<TARGET_FILE:freertos_thermostat>")
add_dependencies(costar_tests
  freertos_ping_pong_1 freertos_ping_pong_3 freertos_errno freertos_link_node freertos_thermostat)

if(TARGET zephyr_ping_pong)
  target_sources(costar_tests PRIVATE zephyr_test.cpp)
  target_compile_definitions(costar_tests PRIVATE
    ZEPHYR_PING_PONG="$<TARGET_FILE:zephyr_ping_pong>")
  add_dependencies(costar_tests zephyr_ping_pong)
  add_test(NAME nsi_link_options
    COMMAND ${COSTAR_ZEPHYR_PYTHON} ${CMAKE_CURRENT_SOURCE_DIR}/nsi_link_options_test.py
            ${PROJECT_SOURCE_DIR}/cmake/costar_nsi_link_options.py ${COSTAR_ZEPHYR_MAKE}
            ${CMAKE_CURRENT_BINARY_DIR}/nsi_link_options)
endif()

add_test(NAME costar_tests COMMAND costar_tests)

# Scenario files with golden traces, run through the costar command.
set(image_dir $<TARGET_FILE_DIR:freertos_link_node>)
add_test(NAME scenarios
  COMMAND costar-cli test ${CMAKE_CURRENT_SOURCE_DIR}/scenarios --images ${image_dir})
if(TARGET zephyr_ping_pong)
  add_test(NAME zephyr_scenarios
    COMMAND costar-cli test ${CMAKE_CURRENT_SOURCE_DIR}/scenarios/zephyr --images ${image_dir})
endif()
