FetchContent_Declare(doctest
  URL https://github.com/doctest/doctest/archive/refs/tags/v2.5.3.tar.gz
  URL_HASH SHA256=174ebc4e769928959614789c5b4e9c3d0a0f81a62bb608756b127bfebfb21331
  DOWNLOAD_EXTRACT_TIMESTAMP ON
  EXCLUDE_FROM_ALL)
FetchContent_MakeAvailable(doctest)

add_executable(costar_tests freertos_test.cpp)
target_link_libraries(costar_tests PRIVATE costar doctest::doctest)
target_compile_definitions(costar_tests PRIVATE
  PING_PONG_1="$<TARGET_FILE:freertos_ping_pong_1>"
  PING_PONG_3="$<TARGET_FILE:freertos_ping_pong_3>")
add_dependencies(costar_tests freertos_ping_pong_1 freertos_ping_pong_3)
add_test(NAME costar_tests COMMAND costar_tests)
