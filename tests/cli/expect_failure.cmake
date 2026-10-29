# Run a command that must fail with a given message on its output.
#   cmake -DCOMMAND="<command;args...>" -DMESSAGE=<regex> -P expect_failure.cmake
execute_process(COMMAND ${COMMAND} RESULT_VARIABLE result OUTPUT_VARIABLE out ERROR_VARIABLE out)
if(result EQUAL 0 OR NOT out MATCHES "${MESSAGE}")
  message(FATAL_ERROR "expected failure with '${MESSAGE}', got exit ${result}:\n${out}")
endif()
