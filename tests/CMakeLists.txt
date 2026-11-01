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
# Failing scenarios leave their traces, console text and golden diffs here;
# CI uploads them.
set(artifacts ${PROJECT_BINARY_DIR}/scenario-artifacts)
costar_add_scenario_tests(scenarios SCENARIOS scenarios IMAGES ${image_dir} ARTIFACTS ${artifacts})
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
  costar_add_scenario_tests(zephyr SCENARIOS scenarios/zephyr IMAGES ${image_dir} LABELS zephyr ARTIFACTS ${artifacts})
  # Bridged, so skipped, but loaded: the scenario and its image must be valid.
  costar_add_scenario_tests(examples/zephyr SCENARIOS ${PROJECT_SOURCE_DIR}/examples/zephyr IMAGES ${image_dir}
                            LABELS zephyr)

  # --format console: the console text alone, after each machine's name.
  add_test(NAME cli_format_console
    COMMAND costar-cli run ${CMAKE_CURRENT_SOURCE_DIR}/scenarios/zephyr/zephyr_and_freertos.toml --images ${image_dir}
            --format console)
  set_tests_properties(cli_format_console PROPERTIES
    PASS_REGULAR_EXPRESSION "^zephyr: received 1 at 0 ms\nzephyr: sent 1 at 0 ms\nzephyr: received 2 at 4 ms\n"
    FAIL_REGULAR_EXPRESSION " m[01] " LABELS zephyr)
  # ... and with --machine, one machine's, without its name.
  add_test(NAME cli_format_console_machine
    COMMAND costar-cli run ${CMAKE_CURRENT_SOURCE_DIR}/scenarios/zephyr/zephyr_and_freertos.toml --images ${image_dir}
            --format console --machine zephyr)
  set_tests_properties(cli_format_console_machine PROPERTIES
    PASS_REGULAR_EXPRESSION "^received 1 at 0 ms\nsent 1 at 0 ms\nreceived 2 at 4 ms\n"
    FAIL_REGULAR_EXPRESSION "zephyr: |freertos: " LABELS zephyr)

  # twister runs an unmodified ztest app on the native_sim/native/64/costar
  # board (ports/zephyr/module/README.md).  It needs a Python with
  # twister's requirements, COSTAR_TWISTER_PYTHON (default: the one for
  # Zephyr builds).
  set(COSTAR_TWISTER_PYTHON ${COSTAR_ZEPHYR_PYTHON} CACHE FILEPATH "Python with twister's requirements")
  execute_process(COMMAND ${COSTAR_TWISTER_PYTHON} -c "import natsort, ply, junitparser, psutil, pykwalify"
                  RESULT_VARIABLE twister_missing OUTPUT_QUIET ERROR_QUIET)
  if(twister_missing)
    message(STATUS "twister test: off (${COSTAR_TWISTER_PYTHON} lacks twister's requirements; set COSTAR_TWISTER_PYTHON)")
  else()
    costar_zephyr_module_setting(module_setting)
    # The app is instrumented (COSTAR_INSTRUMENT), so its image links busy_poll.c too.
    add_test(NAME twister_ztest
      COMMAND ${COSTAR_TWISTER_PYTHON} ${COSTAR_ZEPHYR_BASE}/scripts/twister -p native_sim/native/64/costar
              --board-root ${PROJECT_SOURCE_DIR}/ports/zephyr/module/boards -x ${module_setting}
              -x COSTAR_EXECUTABLE=$<TARGET_FILE:costar-cli> -x COSTAR_INSTRUMENT=app
              -T ${PROJECT_SOURCE_DIR}/firmware/zephyr_ztest
              -O ${CMAKE_CURRENT_BINARY_DIR}/twister --clobber-output --inline-logs -j 1)
    set_tests_properties(twister_ztest PROPERTIES
      ENVIRONMENT "ZEPHYR_BASE=${COSTAR_ZEPHYR_BASE};ZEPHYR_TOOLCHAIN_VARIANT=host"
      PASS_REGULAR_EXPRESSION "1 of 1 executed test configurations passed.*7 of 7 executed test cases passed"
      LABELS zephyr TIMEOUT 600)
  endif()
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
# CI output: what failing scenarios leave behind (--artifacts), and JUnit.
if(TARGET zephyr_uart_flash)
  set(zephyr_artifacts -DZEPHYR=ON)
endif()
add_test(NAME cli_artifacts
  COMMAND ${CMAKE_COMMAND} -DCOSTAR=$<TARGET_FILE:costar-cli> -DSOURCE=${CMAKE_CURRENT_SOURCE_DIR} -DIMAGES=${image_dir}
          -DDIR=${CMAKE_CURRENT_BINARY_DIR}/cli_artifacts ${zephyr_artifacts}
          -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/artifacts.cmake)
set_tests_properties(cli_artifacts PROPERTIES TIMEOUT 60)
add_test(NAME cli_artifacts_need_dir
  COMMAND costar-cli test ${CMAKE_CURRENT_SOURCE_DIR}/scenarios/ping_pong.toml --all-artifacts)
add_test(NAME cli_run_junit COMMAND costar-cli run ${CMAKE_CURRENT_SOURCE_DIR}/scenarios/ping_pong.toml --junit x.xml)
set_tests_properties(cli_artifacts_need_dir cli_run_junit PROPERTIES WILL_FAIL TRUE)
# Sweeps: every combination runs, and a failing one says how to reproduce it.
add_test(NAME cli_sweep
  COMMAND ${CMAKE_COMMAND} -DCOSTAR=$<TARGET_FILE:costar-cli> -DSCENARIO=${CMAKE_CURRENT_SOURCE_DIR}/scenarios/link_sweep.toml
          -DIMAGES=${image_dir} -DDIR=${CMAKE_CURRENT_BINARY_DIR}/cli_sweep -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/sweep.cmake)
add_test(NAME cli_set_unknown_path
  COMMAND ${CMAKE_COMMAND} "-DCOMMAND=$<TARGET_FILE:costar-cli>;run;${CMAKE_CURRENT_SOURCE_DIR}/scenarios/ping_pong.toml;--set;wire.2.latency_us=1"
          "-DMESSAGE=ping_pong.toml: --set wire.2.latency_us: wire has 1 element\\(s\\), not 2"
          -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/expect_failure.cmake)
add_test(NAME cli_set_needs_value COMMAND costar-cli run ${CMAKE_CURRENT_SOURCE_DIR}/scenarios/ping_pong.toml --set duration_ms)
set_tests_properties(cli_set_needs_value PROPERTIES WILL_FAIL TRUE)
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

# Breaking into a debugger (docs/DEBUGGING.md).  Without one, a break is
# reported and the run carries on unchanged.
add_test(NAME cli_break_without_debugger
  COMMAND costar-cli run ${CMAKE_CURRENT_SOURCE_DIR}/cli/ping_pong_firmware.toml --images ${image_dir}
          --break-on board:received=2 --break-at-ms 3)
set_tests_properties(cli_break_without_debugger PROPERTIES PASS_REGULAR_EXPRESSION
  "costar: break on 1000000 m0 received 2 \\(no debugger attached: carrying on\\).*costar: break at 3000000 ns \\(no debugger attached.*5000000 m0 sender_done 5")
# A break at an instant comes before the scenario's inputs due then.
add_test(NAME cli_break_at_before_inputs
  COMMAND costar-cli run ${CMAKE_CURRENT_SOURCE_DIR}/cli/break_at_input.toml --break-at-ms 1)
set_tests_properties(cli_break_at_before_inputs PROPERTIES PASS_REGULAR_EXPRESSION
  "costar: break at 1000000 ns \\(no debugger attached: carrying on\\)\n1000000 m0 set gpio\\[0\\]=7")
add_test(NAME cli_break_on_bad_spec
  COMMAND costar-cli run ${CMAKE_CURRENT_SOURCE_DIR}/cli/ping_pong_firmware.toml --images ${image_dir} --break-on board:)
set_tests_properties(cli_break_on_bad_spec PROPERTIES WILL_FAIL TRUE)
# Under GDB, with tools/gdb/costar.py: the break stops in the firmware, and
# the extension lists machines and RTOS threads and shows suspended threads'
# stacks.
find_program(COSTAR_GDB gdb)
if(COSTAR_GDB)
  set(gdb_run ${CMAKE_COMMAND} -DGDB=${COSTAR_GDB} -DEXTENSION=${PROJECT_SOURCE_DIR}/tools/gdb/costar.py)
  set(ping_pong "$<TARGET_FILE:costar-cli>;run;${CMAKE_CURRENT_SOURCE_DIR}/cli/ping_pong_firmware.toml;--images;${image_dir}")
  add_test(NAME gdb_break_on
    COMMAND ${gdb_run} -DSCRIPT=${CMAKE_CURRENT_SOURCE_DIR}/cli/gdb_break_on.gdb
            "-DCOMMAND=${ping_pong};--break-on;board:received=2"
            "-DEXPECT=costar: break on 1000000 m0 received 2\n;SIGTRAP;#[0-9]+ +0x[0-9a-f]+ in costar_trace \\(;#[0-9]+ +0x[0-9a-f]+ in prvReceiver \\(;\\* board +m0 +1000000 ns +cpu awake +5 threads;running here +receiver +FreeRTOS task prvReceiver\n;suspended +sender +FreeRTOS task prvSender +in [^\n]*xQueueGenericSend;suspended +IDLE +FreeRTOS task prvIdleTask;in xQueueGenericSend \\(.*in prvSender \\(;exited normally"
            -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/gdb.cmake)
  add_test(NAME gdb_break_at
    COMMAND ${gdb_run} -DSCRIPT=${CMAKE_CURRENT_SOURCE_DIR}/cli/gdb_break_at.gdb
            "-DCOMMAND=${ping_pong};--break-at-ms;3"
            "-DEXPECT=costar: break at 3000000 ns\n;  board +m0 +3000000 ns +cpu halted;suspended +receiver +FreeRTOS task prvReceiver +in [^\n]*xQueueReceive;current +IDLE +FreeRTOS task prvIdleTask +in ;in xQueueReceive \\(.*in prvReceiver \\(;3000000 m0 received 4;exited normally"
            -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/gdb.cmake)
  set(gdb_tests gdb_break_on gdb_break_at)
  if(TARGET zephyr_ping_pong)
    add_test(NAME gdb_zephyr
      COMMAND ${gdb_run} -DSCRIPT=${CMAKE_CURRENT_SOURCE_DIR}/cli/gdb_zephyr.gdb
              "-DCOMMAND=$<TARGET_FILE:costar-cli>;run;${CMAKE_CURRENT_SOURCE_DIR}/scenarios/zephyr/zephyr_and_freertos.toml;--images;${image_dir};--break-on;zephyr:log"
              "-DEXPECT=costar: break on 0 m0 log received 1 at 0 ms\n;in receiver \\(;\\* zephyr +m0 +0 ns +cpu awake +4 threads;  freertos +m1 +0 ns +cpu awake +1 thread;running here +- +Zephyr thread;suspended +- +Zephyr thread \\(slot [0-9]\\) +in nct_swap_threads;not started +- +costar::;20000000 m0 stop 3;exited normally"
              -P ${CMAKE_CURRENT_SOURCE_DIR}/cli/gdb.cmake)
    list(APPEND gdb_tests gdb_zephyr)
  endif()
  # No queries to debuginfod servers; LeakSanitizer can't run under a debugger.
  set_tests_properties(${gdb_tests} PROPERTIES TIMEOUT 120 ENVIRONMENT "DEBUGINFOD_URLS=;ASAN_OPTIONS=detect_leaks=0")
endif()

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
