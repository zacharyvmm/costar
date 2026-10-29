#define DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN
#include <doctest/doctest.h>

#include <vector>

#include "costar/world.hpp"

using costar::milliseconds;
using costar::Time;
using costar::TraceRecord;
using costar::World;

namespace {

// What firmware/freertos_ping_pong must produce on machine `m`.  The receiver
// has the higher priority, so each send preempts the sender before it traces.
std::vector<TraceRecord> expected_ping_pong(uint32_t m, uint64_t period_ticks) {
  std::vector<TraceRecord> out;
  for (uint32_t i = 1; i <= 5; ++i) {
    const Time at = milliseconds((i - 1) * period_ticks);
    out.push_back({at, m, "received", i});
    out.push_back({at, m, "sent", i});
  }
  out.push_back({milliseconds(5 * period_ticks), m, "sender_done", 5});
  return out;
}

}  // namespace

TEST_CASE("unmodified FreeRTOS schedules a queue ping-pong") {
  SUBCASE("1-tick period (idle halts until the next tick)") {
    World world;
    world.add_machine(PING_PONG_1);
    world.run_until(milliseconds(20));
    CHECK(world.trace().records() == expected_ping_pong(0, 1));
  }
  SUBCASE("3-tick period (tickless idle)") {
    World world;
    world.add_machine(PING_PONG_3);
    world.run_until(milliseconds(20));
    CHECK(world.trace().records() == expected_ping_pong(0, 3));
  }
}

TEST_CASE("two copies of one image are independent kernels") {
  World world;
  world.add_machine(PING_PONG_1);
  world.add_machine(PING_PONG_1);
  world.run_until(milliseconds(20));
  CHECK(world.trace().for_machine(0) == expected_ping_pong(0, 1));
  CHECK(world.trace().for_machine(1) == expected_ping_pong(1, 1));
}

TEST_CASE("different images run side by side") {
  World world;
  world.add_machine(PING_PONG_1);
  world.add_machine(PING_PONG_3);
  world.run_until(milliseconds(20));
  CHECK(world.trace().for_machine(0) == expected_ping_pong(0, 1));
  CHECK(world.trace().for_machine(1) == expected_ping_pong(1, 3));
}

TEST_CASE("run() stops once every machine waits without a timeout") {
  World world;
  world.add_machine(PING_PONG_3);
  world.run();
  CHECK(world.trace().records() == expected_ping_pong(0, 3));
  // The last event is the tick after the sender deleted itself.
  CHECK(world.now() <= milliseconds(5 * 3 + 1));
}

TEST_CASE("runs are deterministic") {
  auto run_once = [] {
    World world;
    world.add_machine(PING_PONG_1);
    world.add_machine(PING_PONG_3);
    world.run_until(milliseconds(20));
    return world.trace().format();
  };
  CHECK(run_once() == run_once());
}

TEST_CASE("worlds can be torn down mid-run, repeatedly") {
  for (int i = 0; i < 50; ++i) {
    World world;
    world.add_machine(PING_PONG_1);
    world.add_machine(PING_PONG_3);
    world.run_until(milliseconds(4));  // tasks are suspended mid-flight
  }
  CHECK(true);
}
