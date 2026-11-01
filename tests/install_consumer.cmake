# Install costar into a scratch prefix, then build and test
# examples/consumer against it.
#   cmake -DBUILD=<costar build> -DSOURCE=<examples/consumer> -DDIR=<scratch> -DGENERATOR=<...>
#         [-DPREFIX_PATH=<more prefixes, separated by |>]
#         [-DCONSUMER_ARGS=<cache settings for the consumer, separated by |>]
#         [-DCONFIG=<configuration, for multi-config generators>] [-DGRPC=<costar built with gRPC>]
#         -P install_consumer.cmake
# Then checks the grpc component: found if costar has gRPC, an error if not;
# and as an optional component, found if costar and gRPC both are, and else
# not, with costar still found.
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

# find_package(costar COMPONENTS grpc).
file(WRITE ${DIR}/grpc/CMakeLists.txt [=[
cmake_minimum_required(VERSION 3.28)
project(costar_grpc_component LANGUAGES C CXX)
find_package(costar 0.2 REQUIRED COMPONENTS grpc)
if(NOT TARGET costar::costar_grpc OR NOT TARGET costar::costar-grpc)
  message(FATAL_ERROR "the grpc component has no targets")
endif()
]=])
execute_process(COMMAND ${CMAKE_COMMAND} -E env CMAKE_PREFIX_PATH=${prefix_path}
                ${CMAKE_COMMAND} -S ${DIR}/grpc -B ${DIR}/grpc/build -G ${GENERATOR} ${consumer_args}
                RESULT_VARIABLE result OUTPUT_VARIABLE out ERROR_VARIABLE out)
if(GRPC AND NOT result EQUAL 0)
  message(FATAL_ERROR "find_package(costar COMPONENTS grpc) failed:\n${out}")
elseif(NOT GRPC AND (result EQUAL 0 OR NOT out MATCHES "built without gRPC"))
  message(FATAL_ERROR "find_package(costar COMPONENTS grpc) should fail without gRPC:\n${out}")
endif()

# find_package(costar REQUIRED OPTIONAL_COMPONENTS grpc), with gRPC there
# (if costar has it) and with it missing.
file(WRITE ${DIR}/optional/CMakeLists.txt [=[
cmake_minimum_required(VERSION 3.28)
project(costar_optional_grpc LANGUAGES C CXX)
find_package(costar 0.2 REQUIRED OPTIONAL_COMPONENTS grpc)
if(costar_grpc_FOUND AND NOT TARGET costar::costar_grpc)
  message(FATAL_ERROR "the grpc component has no targets")
endif()
message(STATUS "costar_grpc_FOUND: ${costar_grpc_FOUND}")
]=])
foreach(grpc_missing IN ITEMS OFF ON)
  file(REMOVE_RECURSE ${DIR}/optional/build)
  execute_process(COMMAND ${CMAKE_COMMAND} -E env CMAKE_PREFIX_PATH=${prefix_path}
                  ${CMAKE_COMMAND} -S ${DIR}/optional -B ${DIR}/optional/build -G ${GENERATOR} ${consumer_args}
                  -DCMAKE_DISABLE_FIND_PACKAGE_gRPC=${grpc_missing}
                  RESULT_VARIABLE result OUTPUT_VARIABLE out ERROR_VARIABLE out)
  if(GRPC AND NOT grpc_missing)
    set(expected TRUE)
  else()
    set(expected FALSE)
  endif()
  if(NOT result EQUAL 0 OR NOT out MATCHES "costar_grpc_FOUND: ${expected}\n")
    message(FATAL_ERROR "find_package(costar OPTIONAL_COMPONENTS grpc) with gRPC missing ${grpc_missing}: "
                        "expected costar found, costar_grpc_FOUND ${expected}:\n${out}")
  endif()
endforeach()
