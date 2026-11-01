# `costar run` with a trace filter, JSON lines and a Perfetto file.
#   cmake -DCOSTAR=<costar-cli> -DSCENARIO=<link_nodes.toml> -DIMAGES=<dir> -DDIR=<scratch dir> -P trace_formats.cmake
file(REMOVE_RECURSE ${DIR})
file(MAKE_DIRECTORY ${DIR})
execute_process(COMMAND ${COSTAR} run ${SCENARIO} --images ${IMAGES} --format jsonl --machine responder
                        --label got_* --label tx --exclude-label tx --perfetto ${DIR}/trace.json
                RESULT_VARIABLE result OUTPUT_VARIABLE out ERROR_VARIABLE err)
set(expected [[{"time_ns":2000000,"machine":"responder","label":"got_ping","value":1}
{"time_ns":12000000,"machine":"responder","label":"got_ping","value":2}
{"time_ns":22000000,"machine":"responder","label":"got_ping","value":3}
]])
if(NOT result EQUAL 0 OR NOT out STREQUAL expected)
  message(FATAL_ERROR "run exited ${result} and wrote:\n${out}${err}")
endif()

# The Perfetto file is JSON: metadata for the scenario and its two machines,
# then the same three records as instant events on the responder's track.
file(READ ${DIR}/trace.json perfetto)
string(JSON events LENGTH "${perfetto}")
if(NOT events EQUAL 8)
  message(FATAL_ERROR "expected 8 events, got ${events}:\n${perfetto}")
endif()
string(JSON name GET "${perfetto}" 3 args name)
string(JSON phase GET "${perfetto}" 5 ph)
string(JSON tid GET "${perfetto}" 5 tid)
string(JSON ts GET "${perfetto}" 7 ts)
string(JSON value GET "${perfetto}" 7 args value)
if(NOT name STREQUAL "responder" OR NOT phase STREQUAL "i" OR NOT tid EQUAL 2 OR NOT ts EQUAL 22000
   OR NOT value EQUAL 3)
  message(FATAL_ERROR "unexpected events (${name} ${phase} ${tid} ${ts} ${value}):\n${perfetto}")
endif()
file(REMOVE_RECURSE ${DIR})
