# Turn a Zephyr native_sim build's runner link inputs (zephyr/NSI/nsi_config:
# NSI_LINK_OPTIONS and NSI_EXTRA_LIBS, e.g. -lstdc++ for C++ applications) into
# two response files for the costar image: compiler-driver flags (OPTIONS) and
# libraries, which must follow the objects (LIBRARIES, read by the linker).
# The linker script (-T ...) is the stock runner's, and -static / -pie
# options are for its executable; they are dropped.  Each argument is quoted
# for response-file syntax (spaces, quotes and backslashes survive).
#   cmake -DNSI_CONFIG=<nsi_config> -DOPTIONS=<file> -DLIBRARIES=<file> -P costar_nsi_link_options.cmake

file(STRINGS ${NSI_CONFIG} lines REGEX "^NSI_(LINK_OPTIONS|EXTRA_LIBS):=")
set(options "")
set(libraries "")
foreach(line IN LISTS lines)
  string(REGEX REPLACE "^NSI_[A-Z_]+:=" "" value "${line}")
  separate_arguments(value UNIX_COMMAND "${value}")
  set(skip_next FALSE)
  foreach(item IN LISTS value)
    if(skip_next)
      set(skip_next FALSE)
    elseif(item STREQUAL "-T")
      set(skip_next TRUE)
    elseif(item MATCHES "^-T" OR item MATCHES "^-(static|static-pie|pie|no-pie)$")
    else()
      string(REPLACE "\\" "\\\\" quoted "${item}")
      string(REPLACE "\"" "\\\"" quoted "${quoted}")
      if(item MATCHES "^-l" OR item MATCHES "\\.(a|so)$")
        string(APPEND libraries "\"${quoted}\"\n")
      else()
        string(APPEND options "\"${quoted}\"\n")
      endif()
    endif()
  endforeach()
endforeach()

# Write on change only, so unchanged builds don't relink (but always create
# the file: the image's link depends on it even when it is empty).
function(write_if_changed file content)
  if(EXISTS ${file})
    file(READ ${file} previous)
    if("${previous}" STREQUAL "${content}")
      return()
    endif()
  endif()
  file(WRITE ${file} "${content}")
endfunction()
write_if_changed(${OPTIONS} "${options}")
write_if_changed(${LIBRARIES} "${libraries}")
