# The Python client's tests (python/tests), run by pytest against
# costar-grpc.  They need a Python with grpcio, grpcio-tools and pytest:
# COSTAR_PYTHON, or else the Python CMake finds (an active virtualenv first).
# Without one, they are left out.
#
# The costar.v1 stubs are generated here, into this build directory; the
# rest of the package is used from the source tree.

set(COSTAR_PYTHON "" CACHE FILEPATH "A Python with grpcio, grpcio-tools and pytest, for the Python client's tests")
if(COSTAR_PYTHON)
  set(python ${COSTAR_PYTHON})
else()
  find_package(Python3 COMPONENTS Interpreter QUIET)
  set(python ${Python3_EXECUTABLE})
endif()
if(python)
  execute_process(COMMAND ${python} -c "import grpc, grpc_tools, pytest" RESULT_VARIABLE missing
                  OUTPUT_QUIET ERROR_QUIET)
endif()
if(NOT python OR missing)
  message(STATUS "Python client tests: off (no Python with grpcio, grpcio-tools and pytest; set COSTAR_PYTHON)")
  return()
endif()
message(STATUS "Python client tests: ${python}")

set(proto ${PROJECT_SOURCE_DIR}/proto/costar/v1/costar.proto)
set(stubs ${CMAKE_CURRENT_BINARY_DIR}/costar/v1)
add_custom_command(
  OUTPUT ${stubs}/costar_pb2.py ${stubs}/costar_pb2_grpc.py ${stubs}/__init__.py
  COMMAND ${CMAKE_COMMAND} -E make_directory ${stubs}
  COMMAND ${python} -m grpc_tools.protoc -I${PROJECT_SOURCE_DIR}/proto --python_out=${CMAKE_CURRENT_BINARY_DIR}
          --grpc_python_out=${CMAKE_CURRENT_BINARY_DIR} costar/v1/costar.proto
  COMMAND ${CMAKE_COMMAND} -E touch ${stubs}/__init__.py
  DEPENDS ${proto}
  COMMENT "Generating the costar.v1 Python stubs")
add_custom_target(costar_python_stubs ALL DEPENDS ${stubs}/costar_pb2.py ${stubs}/costar_pb2_grpc.py ${stubs}/__init__.py)

# The plugin is loaded from the source tree, not from an installed costar
# package that might be older.
add_test(NAME python
  COMMAND ${python} -m pytest ${CMAKE_CURRENT_SOURCE_DIR}/tests -q -p costar.pytest_plugin -p no:cacheprovider
          --costar-grpc $<TARGET_FILE:costar-grpc> --costar-images $<TARGET_FILE_DIR:freertos_link_node>)
set_tests_properties(python PROPERTIES
  ENVIRONMENT "PYTHONPATH=${CMAKE_CURRENT_SOURCE_DIR}/src:${CMAKE_CURRENT_BINARY_DIR};PYTEST_DISABLE_PLUGIN_AUTOLOAD=1;PYTHONDONTWRITEBYTECODE=1"
  TIMEOUT 300)
