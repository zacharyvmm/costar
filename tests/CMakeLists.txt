FetchContent_Declare(doctest
  URL https://github.com/doctest/doctest/archive/refs/tags/v2.5.3.tar.gz
  URL_HASH SHA256=174ebc4e769928959614789c5b4e9c3d0a0f81a62bb608756b127bfebfb21331
  DOWNLOAD_EXTRACT_TIMESTAMP ON
  EXCLUDE_FROM_ALL)
FetchContent_MakeAvailable(doctest)

add_executable(costar_tests main.cpp freertos_test.cpp devices_test.cpp host_test.cpp models_test.cpp scenario_test.cpp
  session_test.cpp trace_test.cpp wires_test.cpp)
target_link_libraries(costar_tests PRIVATE costar doctest::doctest)
costar_instrument(costar_tests)
target_compile_definitions(costar_tests PRIVATE
  PING_PONG_1="$<TARGET_FILE:freertos_ping_pong_1>"
  PING_PONG_3="$<TARGET_FILE:freertos_ping_pong_3>"
  ERRNO_IMAGE="$<TARGET_FILE:freertos_errno>"
  LINK_NODE="$<TARGET_FILE:freertos_link_node>"
  THERMOSTAT="$<TARGET_FILE:freertos_thermostat>"
  KERNEL_PROBE="$<TARGET_FILE:freertos_kernel_probe>"
  STALL_IMAGE="$<TARGET_FILE:freertos_stall>"
  STACK_IMAGE="$<TARGET_FILE:freertos_stack_overflow>"
  DEADLINE_IMAGE="$<TARGET_FILE:freertos_deadline>"
  DEADLINE_APP_IMAGE="$<TARGET_FILE:freertos_deadline_app>"
  WATCHDOG_IMAGE="$<TARGET_FILE:freertos_watchdog>")
add_dependencies(costar_tests
  freertos_ping_pong_1 freertos_ping_pong_3 freertos_errno freertos_link_node freertos_thermostat
  freertos_kernel_probe freertos_stall freertos_busy_loop_plain freertos_memory_bug freertos_stack_overflow
  freertos_deadline freertos_deadline_app freertos_watchdog)

if(TARGET zephyr_ping_pong)
  target_sources(costar_tests PRIVATE zephyr_test.cpp)
  target_compile_definitions(costar_tests PRIVATE
    ZEPHYR_PING_PONG="$<TARGET_FILE:zephyr_ping_pong>"
    ZEPHYR_UART_FLASH="$<TARGET_FILE:zephyr_uart_flash>"
    ZEPHYR_STACK_OVERFLOW="$<TARGET_FILE:zephyr_stack_overflow>"
    ZEPHYR_ADC_PWM_EEPROM="$<TARGET_FILE:zephyr_adc_pwm_eeprom>")
  add_dependencies(costar_tests zephyr_ping_pong zephyr_uart_flash zephyr_stack_overflow zephyr_adc_pwm_eeprom)
  add_test(NAME nsi_link_options
    COMMAND ${COSTAR_ZEPHYR_PYTHON} ${CMAKE_CURRENT_SOURCE_DIR}/nsi_link_options_test.py
            ${PROJECT_SOURCE_DIR}/cmake/costar_nsi_link_options.py ${COSTAR_ZEPHYR_MAKE}
            ${CMAKE_CURRENT_BINARY_DIR}/nsi_link_options)
endif()

if(TARGET costar_grpc)
  target_sources(costar_tests PRIVATE grpc_test.cpp)
  target_link_libraries(costar_tests PRIVATE costar_grpc)
  target_compile_definitions(costar_tests PRIVATE SCENARIO_DIR="${CMAKE_CURRENT_SOURCE_DIR}/scenarios")
endif()

add_test(NAME costar_tests COMMAND costar_tests)

# Scenario files with golden traces, run through the costar command: one test
# per scenario.  The example's host bridge makes it a skipped one.
set(image_dir $<TARGET_FILE_DIR:freertos_link_node>)
costar_add_scenario_tests(scenarios SCENARIOS scenarios IMAGES ${image_dir})
costar_add_scenario_tests(examples SCENARIOS ${PROJECT_SOURCE_DIR}/examples IMAGES ${image_dir})
# And all of them through one costar test.
add_test(NAME scenario_directory
  COMMAND costar-cli test ${CMAKE_CURRENT_SOURCE_DIR}/scenarios --images ${image_dir} --timeout-s 60)
# A firmware bug can hang a run (a busy loop that is never preempted).
set_tests_properties(costar_tests scenario_directory PROPERTIES TIMEOUT 300)
# The costar command fails when it should.
add_test(NAME cli_missing_golden
  COMMAND costar-cli test ${CMAKE_CURRENT_SOURCE_DIR}/cli/missing_golden.toml)
add_test(NAME cli_bad_until
  COMMAND costar-cli run ${CMAKE_CURRENT_SOURCE_DIR}/scenarios/ping_pong.toml --until-ms 2garbage)
add_test(NAME cli_directory_as_scenario COMMAND costar-cli run ${CMAKE_CURRENT_SOURCE_DIR}/cli)
add_test(NAME cli_nothing_compared COMMAND costar-cli test ${CMAKE_CURRENT_SOURCE_DIR}/cli/no_expect.toml)
add_test(NAME cli_bad_thermal_plant COMMAND costar-cli run ${CMAKE_CURRENT_SOURCE_DIR}/cli/bad_thermal.toml)
set_tests_properties(cli_missing_golden cli_bad_until cli_directory_as_scenario cli_nothing_compared
                     cli_bad_thermal_plant PROPERTIES WILL_FAIL TRUE)
foreach(command test run)
  add_test(NAME cli_${command}_failed_expectation
    COMMAND ${CMAKE_COMMAND} "-DCOMMAND=$<TARGET_FILE:costar-cli>;${command};${CMAKE_CURRENT_SOURCE_DIR}/cli/failed_expectation.toml"
            "-DMESSAGE=expect.event 1: expected any machine rx at or before 0 ns, but it never happened"
            -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/expect_failure.cmake)
endforeach()
# Firmware that stalls, or hangs in a busy loop, fails the run with a reason.
add_test(NAME cli_stall
  COMMAND ${CMAKE_COMMAND} "-DCOMMAND=$<TARGET_FILE:costar-cli>;run;${CMAKE_CURRENT_SOURCE_DIR}/cli/stall.toml;--images;${image_dir}"
          "-DMESSAGE=machine poller stalled at 0 ns: its firmware made more than 1000000 costar calls without letting time pass"
          -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/expect_failure.cmake)
add_test(NAME cli_run_timeout
  COMMAND ${CMAKE_COMMAND} "-DCOMMAND=$<TARGET_FILE:costar-cli>;run;${CMAKE_CURRENT_SOURCE_DIR}/cli/hang.toml;--images;${image_dir};--timeout-s;1"
          "-DMESSAGE=^costar: hang: timed out after 1 s of wall-clock time[^\n]*\n0 m1 rx link 6869\n$"
          -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/expect_failure.cmake)
add_test(NAME cli_test_timeout
  COMMAND ${CMAKE_COMMAND} "-DCOMMAND=$<TARGET_FILE:costar-cli>;test;${CMAKE_CURRENT_SOURCE_DIR}/cli/stall.toml;${CMAKE_CURRENT_SOURCE_DIR}/cli/hang.toml;--images;${image_dir};--timeout-s;1"
          "-DMESSAGE=FAIL stall\n  machine poller stalled.*\nFAIL hang\n  timed out after 1 s"
          -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/expect_failure.cmake)
set_tests_properties(cli_run_timeout cli_test_timeout PROPERTIES TIMEOUT 30)
# A task that overruns its stack fails the run; --stacks reports every thread.
add_test(NAME cli_stack_overflow
  COMMAND ${CMAKE_COMMAND} "-DCOMMAND=$<TARGET_FILE:costar-cli>;run;${CMAKE_CURRENT_SOURCE_DIR}/cli/stack_overflow.toml;--images;${image_dir};--stacks"
          "-DMESSAGE=costar: machine parser, thread 'parser': [0-9]+ bytes of stack used, 10240 allowed [(]1024 declared[)]\n.*costar: machine parser: thread 'parser' overran its stack at 0 ns: it used [0-9]+ bytes, more than the 10240 allowed for its 1024-byte stack"
          -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/expect_failure.cmake)
# Through costar_add_scenario_tests, a stall, a hang and a stack overrun are failed tests.
costar_add_scenario_tests(cli SCENARIOS cli/heap_api.toml IMAGES ${image_dir})
costar_add_scenario_tests(failing SCENARIOS cli/stall.toml cli/hang.toml cli/stack_overflow.toml IMAGES ${image_dir}
                          TIMEOUT 1)
set_tests_properties(failing/stall failing/hang failing/stack_overflow PROPERTIES WILL_FAIL TRUE)
# AddressSanitizer builds catch firmware overrunning a buffer, on the heap
# and on a task's stack, and writing before a heap block (into heap_costar's
# poisoned header).
if(COSTAR_SANITIZE MATCHES "address")
  foreach(bug heap_overrun:heap-buffer-overflow stack_overrun:stack-buffer-overflow heap_underrun:use-after-poison)
    string(REPLACE ":" ";" bug ${bug})
    list(GET bug 0 scenario)
    list(GET bug 1 report)
    add_test(NAME asan_${scenario}
      COMMAND ${CMAKE_COMMAND} "-DCOMMAND=$<TARGET_FILE:costar-cli>;run;${CMAKE_CURRENT_SOURCE_DIR}/cli/${scenario}.toml;--images;${image_dir}"
              "-DMESSAGE=AddressSanitizer: ${report}"
              -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/expect_failure.cmake)
  endforeach()
endif()
add_test(NAME cli_bad_timeout
  COMMAND costar-cli run ${CMAKE_CURRENT_SOURCE_DIR}/scenarios/ping_pong.toml --timeout-s 0)
set_tests_properties(cli_bad_timeout PROPERTIES WILL_FAIL TRUE)
add_test(NAME cli_empty_golden COMMAND costar-cli test ${CMAKE_CURRENT_SOURCE_DIR}/cli/empty_golden.toml)
add_test(NAME cli_write_trace
  COMMAND ${CMAKE_COMMAND} -DCOSTAR=$<TARGET_FILE:costar-cli> -DSCENARIO=${CMAKE_CURRENT_SOURCE_DIR}/scenarios/ping_pong.toml
          -DDIR=${CMAKE_CURRENT_BINARY_DIR}/cli_write_trace -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/write_trace.cmake)

# A Zephyr image that stops being instrumented is reconfigured without it.
if(TARGET zephyr_busy_loop)
  add_test(NAME zephyr_instrument_removed
    COMMAND ${CMAKE_COMMAND} -DSOURCE=${PROJECT_SOURCE_DIR} -DDIR=${CMAKE_CURRENT_BINARY_DIR}/zephyr_instrument_removed
            "-DGENERATOR=${CMAKE_GENERATOR}" -DMAKE_PROGRAM=${CMAKE_MAKE_PROGRAM} -DCOMPILER=${CMAKE_C_COMPILER}
            -DZEPHYR_BASE=${COSTAR_ZEPHYR_BASE} -DPYTHON=${COSTAR_ZEPHYR_PYTHON}
            -P ${CMAKE_CURRENT_SOURCE_DIR}/zephyr_instrument_removed.cmake)
  set_tests_properties(zephyr_instrument_removed PROPERTIES LABELS zephyr)
endif()

if(TARGET zephyr_ping_pong)
  costar_add_scenario_tests(zephyr SCENARIOS scenarios/zephyr IMAGES ${image_dir} LABELS zephyr)
  # Bridged, so skipped, but loaded: the scenario and its image must be valid.
  costar_add_scenario_tests(examples/zephyr SCENARIOS ${PROJECT_SOURCE_DIR}/examples/zephyr IMAGES ${image_dir}
                            LABELS zephyr)
endif()

# Trace output: filters, JSON lines, Perfetto, and golden diffs.
add_test(NAME cli_trace_formats
  COMMAND ${CMAKE_COMMAND} -DCOSTAR=$<TARGET_FILE:costar-cli> -DSCENARIO=${CMAKE_CURRENT_SOURCE_DIR}/scenarios/link_nodes.toml
          -DIMAGES=${image_dir} -DDIR=${CMAKE_CURRENT_BINARY_DIR}/cli_trace_formats
          -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/trace_formats.cmake)
add_test(NAME cli_golden_diff
  COMMAND ${CMAKE_COMMAND} "-DCOMMAND=$<TARGET_FILE:costar-cli>;test;${CMAKE_CURRENT_SOURCE_DIR}/cli/golden_diff.toml;--images;${image_dir}"
          "-DMESSAGE=golden_diff.trace: the trace differs \\(- golden, \\+ actual\\):
    @@ -1,7 \\+1,7 @@
     0 m1 set node\\[0\\]=1
.*
    -2000000 m1 got_ping 9
    \\+2000000 m1 got_ping 1
.*
    @@ -14,5 \\+14,6 @@
.*
    \\+22000000 m1 tx link 5203
"
          -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/expect_failure.cmake)
add_test(NAME cli_bad_format COMMAND costar-cli run ${CMAKE_CURRENT_SOURCE_DIR}/scenarios/ping_pong.toml --format xml)
add_test(NAME cli_filter_unknown_machine
  COMMAND costar-cli run ${CMAKE_CURRENT_SOURCE_DIR}/scenarios/ping_pong.toml --machine nobody)
add_test(NAME cli_test_perfetto COMMAND costar-cli test ${CMAKE_CURRENT_SOURCE_DIR}/scenarios/ping_pong.toml --perfetto x.json)
set_tests_properties(cli_bad_format cli_filter_unknown_machine cli_test_perfetto PROPERTIES WILL_FAIL TRUE)

# Consoles: standard input and output on a firmware UART (costar run --console).
# Output is the UART's raw text; the trace goes only to --trace.
add_test(NAME cli_console_echo
  COMMAND ${CMAKE_COMMAND} "-DCOMMAND=$<TARGET_FILE:costar-cli>;run;${CMAKE_CURRENT_SOURCE_DIR}/cli/console_echo.toml;--images;${image_dir};--console;echo.uart"
          "-DINPUT=hello\n" "-DOUTPUT=^HELLO\n$"
          -DTRACE=${CMAKE_CURRENT_BINARY_DIR}/cli_console_echo.trace "-DTRACE_OUTPUT=^0 m0 rx uart 68656c6c6f0a\n0 m0 tx uart 48454c4c4f0a\n$"
          -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/console.cmake)
add_test(NAME cli_console_bad_endpoint
  COMMAND ${CMAKE_COMMAND} "-DCOMMAND=$<TARGET_FILE:costar-cli>;run;${CMAKE_CURRENT_SOURCE_DIR}/cli/console_echo.toml;--images;${image_dir};--console;echo.flash"
          "-DMESSAGE=--console: machine 'echo' has no channel 'flash'"
          -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/expect_failure.cmake)
if(TARGET zephyr_shell)
  # Commands typed once the shell is up: it drops what came before, as on a board.
  add_test(NAME cli_console_zephyr_shell
    COMMAND ${CMAKE_COMMAND} "-DCOMMAND=$<TARGET_FILE:costar-cli>;run;${CMAKE_CURRENT_SOURCE_DIR}/cli/zephyr_shell.toml;--images;${image_dir};--console;zephyr.uart"
            "-DINPUT=kernel version\ncostar hello ctest\n" -DDELAY=1
            "-DOUTPUT=uart:~\\$ kernel version\r\nZephyr version [0-9]+\\.[0-9]+\\.[0-9]+\r\n.*hello from ctest at [0-9]+ ms\r\n"
            -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/console.cmake)
  set_tests_properties(cli_console_zephyr_shell PROPERTIES LABELS zephyr)
endif()
find_package(Python3 COMPONENTS Interpreter QUIET)
if(Python3_Interpreter_FOUND)
  add_test(NAME cli_console_terminal
    COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/cli/console_test.py $<TARGET_FILE:costar-cli>
            ${CMAKE_CURRENT_SOURCE_DIR}/cli/console_echo.toml ${image_dir})
  set_tests_properties(cli_console_terminal PROPERTIES TIMEOUT 60)
endif()
set_tests_properties(cli_console_echo cli_console_bad_endpoint PROPERTIES TIMEOUT 30)

# costar installed, and used by a project of its own (examples/consumer).
if(COSTAR_INSTALL)
  string(REPLACE ";" "|" prefix_path "${CMAKE_PREFIX_PATH}")  # where Boost was found, say
  add_test(NAME install_consumer
    COMMAND ${CMAKE_COMMAND} -DBUILD=${PROJECT_BINARY_DIR} -DSOURCE=${PROJECT_SOURCE_DIR}/examples/consumer
            "-DPREFIX_PATH=${prefix_path}"
            -DDIR=${CMAKE_CURRENT_BINARY_DIR}/install_consumer "-DGENERATOR=${CMAKE_GENERATOR}"
            -DCMAKE_CTEST_COMMAND=${CMAKE_CTEST_COMMAND} -DCONFIG=$<CONFIG>
            # Built as costar was: instrumented libraries need instrumented programs.
            "-DCONSUMER_ARGS=-DCMAKE_C_COMPILER=${CMAKE_C_COMPILER}|-DCMAKE_CXX_COMPILER=${CMAKE_CXX_COMPILER}|-DCOSTAR_SANITIZE=${COSTAR_SANITIZE}|-DCOSTAR_COVERAGE=${COSTAR_COVERAGE}"
            -P ${CMAKE_CURRENT_SOURCE_DIR}/install_consumer.cmake)
  set_tests_properties(install_consumer PROPERTIES TIMEOUT 600)
endif()
