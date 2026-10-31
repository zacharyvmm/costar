# Install costar into a scratch prefix, then build and test
# examples/consumer against it.
#   cmake -DBUILD=<costar build> -DSOURCE=<examples/consumer> -DDIR=<scratch> -DGENERATOR=<...>
#         [-DPREFIX_PATH=<more prefixes, separated by |>]
#         [-DCONSUMER_ARGS=<cache settings for the consumer, separated by |>]
#         [-DCONFIG=<configuration, for multi-config generators>] -P install_consumer.cmake
file(REMOVE_RECURSE ${DIR})
function(run)
  execute_process(COMMAND ${ARGN} RESULT_VARIABLE result OUTPUT_VARIABLE out ERROR_VARIABLE out)
  if(NOT result EQUAL 0)
    message(FATAL_ERROR "${ARGN} failed:\n${out}")
  endif()
endfunction()
set(config)
set(test_config)
if(CONFIG)
  set(config --config ${CONFIG})
  set(test_config -C ${CONFIG})
  list(APPEND CONSUMER_ARGS_LIST -DCMAKE_BUILD_TYPE=${CONFIG})  # single-config generators
endif()
run(${CMAKE_COMMAND} --install ${BUILD} --prefix ${DIR}/prefix ${config})
# Through the environment: a list can't be one execute_process() argument.
string(REPLACE "|" ":" prefix_path "${DIR}/prefix|${PREFIX_PATH}")
string(REPLACE "|" ";" consumer_args "${CONSUMER_ARGS}")
list(APPEND consumer_args ${CONSUMER_ARGS_LIST})
run(${CMAKE_COMMAND} -E env CMAKE_PREFIX_PATH=${prefix_path}
    ${CMAKE_COMMAND} -S ${SOURCE} -B ${DIR}/build -G ${GENERATOR} ${consumer_args})
run(${CMAKE_COMMAND} --build ${DIR}/build ${config})
run(${CMAKE_CTEST_COMMAND} --test-dir ${DIR}/build --output-on-failure ${test_config})
