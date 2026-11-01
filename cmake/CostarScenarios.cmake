# Register scenario files as tests: one ctest per scenario.
#
#   costar_add_scenario_tests(<prefix>
#       SCENARIOS <scenario.toml | directory>...   # a directory: its *.toml files
#       [IMAGES <directory>...]                    # where images are found (--images)
#       [COMMAND <target or program>]              # default: costar's own costar command
#       [TIMEOUT <seconds>]                        # wall-clock budget per scenario (default 60)
#       [ARTIFACTS <directory>]                    # what failing scenarios leave (--artifacts)
#       [LABELS <label>...])
#
# Each scenario becomes the test <prefix>/<file name without .toml>, which
# runs `costar test` on it.  Since every scenario is a process of its own,
# `ctest -j` runs them in parallel, firmware that crashes takes only its own
# scenario down, and `ctest --output-junit <file>` writes a JUnit report.
#
# A scenario that `costar test` skips (nothing in [expect], or host bridges)
# is reported as skipped.  With ARTIFACTS, a failing scenario writes its
# trace, console text, golden diff and a summary into
# <directory>/<scenario>-<hash>/ (see `costar test --artifacts`), for CI to upload.  A scenario that runs out of time fails with
# costar's own timeout message; ctest's TIMEOUT, 30 s later, is a backstop.
#
# COMMAND is for programs that register their own plants (see
# costar::cli::main): pass that executable's target.  New scenario files in a
# directory are picked up at the next build.

function(costar_add_scenario_tests prefix)
  cmake_parse_arguments(ARG "" "COMMAND;TIMEOUT;ARTIFACTS" "SCENARIOS;IMAGES;LABELS" ${ARGN})
  if(NOT ARG_SCENARIOS)
    message(FATAL_ERROR "costar_add_scenario_tests(${prefix}): no SCENARIOS given")
  endif()
  if(NOT ARG_COMMAND)
    if(TARGET costar-cli)
      set(ARG_COMMAND costar-cli)
    elseif(TARGET costar::costar-cli)
      set(ARG_COMMAND costar::costar-cli)
    else()
      message(FATAL_ERROR "costar_add_scenario_tests(${prefix}): no costar command; pass COMMAND")
    endif()
  endif()
  if(NOT ARG_TIMEOUT)
    set(ARG_TIMEOUT 60)
  endif()

  set(files)
  foreach(path IN LISTS ARG_SCENARIOS)
    cmake_path(ABSOLUTE_PATH path BASE_DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR})
    if(IS_DIRECTORY ${path})
      file(GLOB found CONFIGURE_DEPENDS ${path}/*.toml)
      list(SORT found)
      list(APPEND files ${found})
    else()
      list(APPEND files ${path})
    endif()
  endforeach()
  if(NOT files)
    message(FATAL_ERROR "costar_add_scenario_tests(${prefix}): no scenario files in ${ARG_SCENARIOS}")
  endif()

  set(image_args)
  foreach(dir IN LISTS ARG_IMAGES)
    list(APPEND image_args --images ${dir})
  endforeach()

  set(artifact_args)
  if(ARG_ARTIFACTS)
    cmake_path(ABSOLUTE_PATH ARG_ARTIFACTS BASE_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR})
    set(artifact_args --artifacts ${ARG_ARTIFACTS})
  endif()

  math(EXPR backstop "${ARG_TIMEOUT} + 30")
  foreach(file IN LISTS files)
    cmake_path(GET file STEM LAST_ONLY stem)
    set(test ${prefix}/${stem})
    if(TEST ${test})
      message(FATAL_ERROR "costar_add_scenario_tests(${prefix}): two scenarios named ${stem}")
    endif()
    add_test(NAME ${test} COMMAND ${ARG_COMMAND} test ${file} ${image_args} --timeout-s ${ARG_TIMEOUT} ${artifact_args})
    set_tests_properties(${test} PROPERTIES
      TIMEOUT ${backstop}
      SKIP_REGULAR_EXPRESSION "^SKIP "
      LABELS "costar;${ARG_LABELS}")
  endforeach()
endfunction()
