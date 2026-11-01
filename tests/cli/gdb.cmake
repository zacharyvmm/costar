# Run a costar command under GDB, in batch mode with tools/gdb/costar.py
# loaded, and check that its output matches every regular expression given.
#   cmake -DGDB=<gdb> -DEXTENSION=<costar.py> -DSCRIPT=<commands.gdb> -DCOMMAND="<command;args...>"
#         -DEXPECT="<regex;regex...>" -P gdb.cmake
execute_process(COMMAND ${GDB} -batch -nx -ex "source ${EXTENSION}" -x ${SCRIPT} --args ${COMMAND}
                RESULT_VARIABLE result OUTPUT_VARIABLE out ERROR_VARIABLE out)
foreach(regex IN LISTS EXPECT)
  if(NOT out MATCHES "${regex}")
    message(FATAL_ERROR "GDB's output doesn't match '${regex}' (exit ${result}):\n${out}")
  endif()
endforeach()
