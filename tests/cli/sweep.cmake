# A sweep with one failing combination: `costar test` runs every combination,
# fails only that one, says how to reproduce it, and lists each in JUnit; the
# command it prints fails the same way.
#   cmake -DCOSTAR=<costar-cli> -DSCENARIO=<link_sweep.toml> -DIMAGES=<dir> -DDIR=<scratch dir> -P sweep.cmake
file(REMOVE_RECURSE ${DIR})
file(MAKE_DIRECTORY ${DIR})
# link_sweep.toml leaves out the 3 ms latency that puts a reply in the fault
# window; put it back.
execute_process(COMMAND ${COSTAR} test ${SCENARIO} --images ${IMAGES} "--set" "sweep.1.values=[1, 2, 3, 4, 5]"
                        --junit ${DIR}/junit.xml
                RESULT_VARIABLE result OUTPUT_VARIABLE out ERROR_VARIABLE err)
string(REGEX MATCHALL "(PASS|FAIL) [^\n]*" lines "${out}")
set(expected
  "PASS link-sweep"
  "PASS link-sweep [wire.initiator.link.latency_ms=1]"
  "PASS link-sweep [wire.initiator.link.latency_ms=2]"
  "FAIL link-sweep [wire.initiator.link.latency_ms=3]"
  "PASS link-sweep [wire.initiator.link.latency_ms=4]"
  "PASS link-sweep [wire.initiator.link.latency_ms=5]")
if(NOT result EQUAL 1 OR NOT "${lines}" STREQUAL "${expected}"
   OR NOT out MATCHES "\n  expect.event 1: expected initiator got_pong 2, but it never happened\n  reproduce: ([^\n]*)\n")
  message(FATAL_ERROR "costar test exited ${result}:\n${out}${err}")
endif()

# The printed command reproduces the failure.
separate_arguments(reproduce UNIX_COMMAND "${CMAKE_MATCH_1}")
execute_process(COMMAND ${reproduce} RESULT_VARIABLE result OUTPUT_VARIABLE trace ERROR_VARIABLE err)
if(NOT result EQUAL 1 OR NOT err MATCHES "expect.event 1: expected initiator got_pong 2, but it never happened"
   OR NOT trace MATCHES "\n3000000 m1 rx link 5001\n")
  message(FATAL_ERROR "${CMAKE_MATCH_1}\nexited ${result}:\n${err}")
endif()

# JUnit: the base configuration and each combination.
file(READ ${DIR}/junit.xml junit)
if(NOT junit MATCHES "tests=\"6\" failures=\"1\" errors=\"0\" skipped=\"0\""
   OR NOT junit MATCHES "name=\"link-sweep \\[wire.initiator.link.latency_ms=3\\]\"[^>]*>\n      <failure message=\"expect.event 1")
  message(FATAL_ERROR "unexpected JUnit report:\n${junit}")
endif()
file(REMOVE_RECURSE ${DIR})
