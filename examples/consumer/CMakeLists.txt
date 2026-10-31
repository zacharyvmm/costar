# A project of its own using an installed costar: FreeRTOS firmware, a costar
# command with the project's own plant, and its scenarios as tests.
#
#   cmake --install <costar build> --prefix <prefix>
#   cmake -S examples/consumer -B build-consumer -DCMAKE_PREFIX_PATH=<prefix>
#   cmake --build build-consumer && ctest --test-dir build-consumer
cmake_minimum_required(VERSION 3.28)
project(costar_consumer LANGUAGES C CXX)

find_package(costar 0.1 REQUIRED)

costar_add_freertos_image(mirror
  SOURCES firmware/main.c
  CONFIG_DIR ${CMAKE_CURRENT_SOURCE_DIR}/firmware)

add_executable(my_costar main.cpp)
target_link_libraries(my_costar PRIVATE costar::costar)
costar_instrument(my_costar)

enable_testing()
costar_add_scenario_tests(consumer
  SCENARIOS scenarios
  IMAGES $<TARGET_FILE_DIR:mirror>
  COMMAND my_costar)
