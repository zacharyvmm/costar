# Pipe INPUT into a console run, and check what it prints on standard output
# (and, with TRACE, the trace it writes).
#   cmake -DCOMMAND=<costar;run;...;--console;machine.device> -DINPUT=<text>
#         [-DDELAY=<seconds before the input>] -DOUTPUT=<regex>
#         [-DTRACE=<file> -DTRACE_OUTPUT=<regex>] -P console.cmake
string(RANDOM LENGTH 8 suffix)
set(input ${CMAKE_CURRENT_BINARY_DIR}/console-input-${suffix}.txt)
file(WRITE ${input} "${INPUT}")
if(TRACE)
  file(REMOVE ${TRACE})
  list(APPEND COMMAND --trace ${TRACE})
endif()
if(DELAY)
  # Input typed once the firmware's console is listening.
  execute_process(COMMAND sh -c "sleep ${DELAY}; cat '${input}'" COMMAND ${COMMAND}
                  RESULTS_VARIABLE results OUTPUT_VARIABLE out ERROR_VARIABLE err)
  list(GET results 1 result)
else()
  execute_process(COMMAND ${COMMAND} INPUT_FILE ${input} RESULT_VARIABLE result OUTPUT_VARIABLE out ERROR_VARIABLE err)
endif()
file(REMOVE ${input})
if(NOT result EQUAL 0 OR NOT out MATCHES "${OUTPUT}")
  message(FATAL_ERROR "expected '${OUTPUT}' on standard output, got exit ${result}:\n${out}\nstandard error:\n${err}")
endif()
if(TRACE)
  file(READ ${TRACE} trace)
  if(NOT trace MATCHES "${TRACE_OUTPUT}")
    message(FATAL_ERROR "expected '${TRACE_OUTPUT}' in the trace, got:\n${trace}")
  endif()
endif()
