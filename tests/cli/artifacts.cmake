# `costar test --artifacts DIR --junit FILE`: what failing scenarios leave
# behind, a timeout included, and the JUnit report.  Also: `--update` writes
# a golden trace through the scenario's golden filter.
#   cmake -DCOSTAR=<costar-cli> -DSOURCE=<tests dir> -DIMAGES=<dir> -DDIR=<scratch dir>
#         [-DZEPHYR=ON] -P artifacts.cmake
file(REMOVE_RECURSE ${DIR})
file(MAKE_DIRECTORY ${DIR})

function(contains file regex)
  file(READ ${file} text)
  if(NOT text MATCHES "${regex}")
    message(FATAL_ERROR "expected '${regex}' in ${file}, got:\n${text}")
  endif()
endfunction()

# The scenario's artifacts directory, <name>-<hash>, in `parent`: exactly one.
function(artifacts_of variable parent name)
  file(GLOB found LIST_DIRECTORIES true ${parent}/${name}-*)
  list(FILTER found INCLUDE REGEX "/${name}-[0-9a-f]+$")
  list(LENGTH found count)
  if(NOT count EQUAL 1)
    message(FATAL_ERROR "expected one artifacts directory for ${name} in ${parent}, got '${found}'")
  endif()
  set(${variable} ${found} PARENT_SCOPE)
endfunction()

# The hang runs last: its timeout ends the process.
execute_process(
  COMMAND ${COSTAR} test ${SOURCE}/cli/golden_diff.toml ${SOURCE}/scenarios/link_nodes.toml
          ${SOURCE}/cli/failed_expectation.toml ${SOURCE}/cli/no_expect.toml ${SOURCE}/cli/hang.toml
          --images ${IMAGES} --artifacts ${DIR}/art --junit ${DIR}/junit.xml --timeout-s 1
  RESULT_VARIABLE result OUTPUT_VARIABLE out ERROR_VARIABLE out)
if(NOT result EQUAL 1 OR NOT out MATCHES "\n  artifacts: [^\n]*/art/golden-diff-[0-9a-f]+\n")
  message(FATAL_ERROR "costar test exited ${result}:\n${out}")
endif()

# A golden mismatch: the whole trace, the whole diff, the would-be golden.
artifacts_of(golden ${DIR}/art golden-diff)
foreach(name trace.txt trace.json diff.txt actual.trace summary.txt)
  if(NOT EXISTS ${golden}/${name})
    message(FATAL_ERROR "golden-diff left no ${name}")
  endif()
endforeach()
contains(${golden}/diff.txt "^@@ -1,7 \\+1,7 @@\n.*\n@@ -14,5 \\+14,6 @@\n")
contains(${golden}/actual.trace "^0 m1 set node\\[0\\]=1\n0 m0 tx link 5001\n")
contains(${golden}/summary.txt "^scenario: golden-diff\nfile: [^\n]*golden_diff.toml\nresult: FAIL\n- [^\n]*golden_diff.trace: the trace differs")
file(READ ${golden}/trace.json perfetto)
string(JSON events LENGTH "${perfetto}")
if(NOT events EQUAL 25)
  message(FATAL_ERROR "trace.json: expected 5 metadata events, 19 records and 1 counter, got ${events}")
endif()
# A failed expectation; a passing scenario leaves nothing.
artifacts_of(failed ${DIR}/art failed-expectation)
contains(${failed}/summary.txt "- expect.event 1: expected any machine rx at or before 0 ns")
file(GLOB passed ${DIR}/art/link-nodes-*)
if(passed)
  message(FATAL_ERROR "a passing scenario left artifacts")
endif()
# A timeout: the trace so far, and why.
artifacts_of(hang ${DIR}/art hang)
contains(${hang}/summary.txt "result: TIMEOUT\n- timed out after 1 s of wall-clock time.*records so far: 1")
contains(${hang}/trace.txt "^0 m1 rx link 6869\n$")

# JUnit: a test case per scenario, with failures' reasons, the timeout included.
contains(${DIR}/junit.xml "<testsuite name=\"costar test\" tests=\"5\" failures=\"3\" errors=\"0\" skipped=\"1\"")
contains(${DIR}/junit.xml "name=\"golden-diff\"[^>]*>\n      <failure message=\"[^\"]*golden_diff.trace: the trace differs \\(- golden, \\+ actual\\):&#10;@@ -1,7 \\+1,7 @@&#10;")
contains(${DIR}/junit.xml "name=\"link-nodes\" file=\"[^\"]*\" time=\"[0-9.]+\"/>")
contains(${DIR}/junit.xml "name=\"no-expect\"[^>]*>\n      <skipped message=\"nothing in \\[expect\\]\"/>")
contains(${DIR}/junit.xml "name=\"hang\"[^>]*>\n      <failure message=\"timed out after 1 s")

# Each machine's console text, with --all-artifacts even when it passes.
if(ZEPHYR)
  execute_process(COMMAND ${COSTAR} test ${SOURCE}/scenarios/zephyr/uart_flash.toml --images ${IMAGES}
                          --artifacts ${DIR}/all --all-artifacts RESULT_VARIABLE result OUTPUT_VARIABLE out)
  if(NOT result EQUAL 0)
    message(FATAL_ERROR "uart_flash failed:\n${out}")
  endif()
  artifacts_of(uart_flash ${DIR}/all zephyr-uart-flash)
  contains(${uart_flash}/zephyr.console.txt "^boot 3\n.*got h at 5086 us\ngot i at 5173 us\n$")
  contains(${uart_flash}/summary.txt "result: PASS\n")
endif()

# Names that look alike once made safe for a directory ("a/b" and "a?b"),
# or one name in two files, don't share artifacts: a passing scenario leaves
# a failing one's alone, and a rerun replaces only its own.
file(MAKE_DIRECTORY ${DIR}/alike/other)
file(READ ${SOURCE}/cli/failed_expectation.toml failing)
string(REPLACE "name = \"failed-expectation\"" "name = \"a/b\"" failing "${failing}")
string(REPLACE "before_ms = 0" "before_ms = 1" passing "${failing}")
file(WRITE ${DIR}/alike/fails.toml "${failing}")
file(WRITE ${DIR}/alike/other/fails.toml "${failing}")
string(REPLACE "name = \"a/b\"" "name = \"a?b\"" passing "${passing}")
file(WRITE ${DIR}/alike/passes.toml "${passing}")
foreach(run 1 2)
  execute_process(COMMAND ${COSTAR} test ${DIR}/alike/fails.toml ${DIR}/alike/other/fails.toml
                          ${DIR}/alike/passes.toml --artifacts ${DIR}/alike/art
                  RESULT_VARIABLE result OUTPUT_VARIABLE out ERROR_VARIABLE out)
  if(NOT result EQUAL 1 OR NOT out MATCHES "FAIL a/b\n.*FAIL a/b\n.*PASS a\\?b\n")
    message(FATAL_ERROR "costar test exited ${result}:\n${out}")
  endif()
  file(GLOB left LIST_DIRECTORIES true RELATIVE ${DIR}/alike/art ${DIR}/alike/art/*)
  if(NOT left MATCHES "^a_b-[0-9a-f]+;a_b-[0-9a-f]+$")
    message(FATAL_ERROR "expected both failures' artifacts, and only theirs, got '${left}'")
  endif()
  set(files "")
  foreach(dir ${left})
    contains(${DIR}/alike/art/${dir}/summary.txt "^scenario: a/b\nfile: [^\n]*fails.toml\nresult: FAIL\n")
    file(STRINGS ${DIR}/alike/art/${dir}/summary.txt file REGEX "^file: ")
    list(APPEND files "${file}")
  endforeach()
  list(SORT files)
  if(NOT files STREQUAL "file: ${DIR}/alike/fails.toml;file: ${DIR}/alike/other/fails.toml")
    message(FATAL_ERROR "expected each file's failure, got '${files}'")
  endif()
endforeach()

# A name too long for a file name is cut short in its directory's name.
string(REPEAT "x" 300 long)
string(REPLACE "name = \"a/b\"" "name = \"${long}\"" long_failing "${failing}")
file(WRITE ${DIR}/long/fails.toml "${long_failing}")
execute_process(COMMAND ${COSTAR} test ${DIR}/long/fails.toml --artifacts ${DIR}/long/art --junit ${DIR}/long/junit.xml
                RESULT_VARIABLE result OUTPUT_VARIABLE out ERROR_VARIABLE out)
file(GLOB left LIST_DIRECTORIES true RELATIVE ${DIR}/long/art ${DIR}/long/art/*)
if(NOT result EQUAL 1 OR NOT left MATCHES "^x+-[0-9a-f]+$" OR NOT EXISTS ${DIR}/long/junit.xml)
  message(FATAL_ERROR "a long name: costar test exited ${result}, left '${left}':\n${out}")
endif()
contains(${DIR}/long/art/${left}/summary.txt "^scenario: ${long}\n")

# --update writes the golden through its filter.
file(MAKE_DIRECTORY ${DIR}/update)
file(COPY ${SOURCE}/scenarios/link_nodes_pings.toml DESTINATION ${DIR}/update)
execute_process(COMMAND ${COSTAR} test ${DIR}/update/link_nodes_pings.toml --images ${IMAGES} --update
                RESULT_VARIABLE result OUTPUT_VARIABLE out ERROR_VARIABLE out)
file(READ ${DIR}/update/link_nodes_pings.trace written)
file(READ ${SOURCE}/scenarios/link_nodes_pings.trace golden)
if(NOT result EQUAL 0 OR NOT written STREQUAL golden)
  message(FATAL_ERROR "--update exited ${result} and wrote:\n${written}${out}")
endif()
file(REMOVE_RECURSE ${DIR})
