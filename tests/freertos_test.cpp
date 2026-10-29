#include <doctest/doctest.h>

#include <vector>

#include "costar/image.hpp"
#include "costar/abi.hpp"
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

TEST_CASE("each task keeps its own errno across switches") {
  World world;
  world.add_machine(ERRNO_IMAGE);
  world.run_until(milliseconds(5));
  CHECK(world.trace().records() == std::vector<TraceRecord>{{0, 0, "a_errno", 11}, {milliseconds(1), 0, "b_errno", 22}});
}

TEST_CASE("an image rejects a host table of another ABI version") {
  costar::Image image(PING_PONG_1);
  costar_host stale = costar::host_table();
  stale.version -= 1;
  CHECK_THROWS(image.attach(stale, nullptr));
}

// ── Kernel probes (firmware/freertos_kernel_probe) ──────────────────────
//
// Each probe is a FreeRTOS behaviour costar_legacy got wrong (its PRs #13,
// #14 and #16): tasks created at runtime, timers, static tasks, interrupts
// in critical sections, deletion.

namespace {

using costar::DeviceKind;
using costar::microseconds;

// A world with one probe machine running `probe`.
struct Probe {
  World world;
  costar::Machine& machine;

  explicit Probe(uint32_t probe)
      : machine(world.add_machine(KERNEL_PROBE, {{.name = "probe", .kind = DeviceKind::Registers},
                                                 {.name = "irq", .kind = DeviceKind::Registers, .irq = 2}})) {
    machine.set_register("probe", 0, probe);
  }

  // What the firmware traced (without the host's register writes).
  std::vector<TraceRecord> firmware_trace() const {
    std::vector<TraceRecord> out;
    for (const TraceRecord& r : world.trace().records()) {
      if (r.label != "set") out.push_back(r);
    }
    return out;
  }
};

}  // namespace

TEST_CASE("probe: runtime tasks, parameters, static tasks and timers") {
  Probe probe(1);
  probe.world.run_until(milliseconds(30));
  CHECK(probe.firmware_trace() == std::vector<TraceRecord>{
                                      {0, 0, "child_param", 42},
                                      {0, 0, "parent_created", 1},
                                      {0, 0, "static_param", 7},
                                      {0, 0, "parent_created", 2},
                                      {milliseconds(5), 0, "timer", 1},
                                      {milliseconds(10), 0, "timer", 2},
                                      {milliseconds(15), 0, "timer", 3},
                                      {milliseconds(20), 0, "parent_done", 3},
                                  });
}

TEST_CASE("probe: an IRQ held off by a critical section runs at unmask and preempts") {
  Probe probe(2);
  probe.world.at(milliseconds(1), [&] { probe.machine.set_register("irq", 0, 1); });
  // Between ticks: the ISR runs on arrival, not at the next tick.
  probe.world.at(microseconds(5500), [&] { probe.machine.set_register("irq", 0, 2); });
  probe.world.run_until(milliseconds(10));
  CHECK(probe.firmware_trace() == std::vector<TraceRecord>{
                                      {0, 0, "low_masked_us", 0},
                                      {milliseconds(2), 0, "low_unmasking_us", 2000},
                                      {milliseconds(2), 0, "high_woken_us", 2000},
                                      {milliseconds(2), 0, "low_after_us", 2000},
                                      {microseconds(5500), 0, "high_woken_us", 5500},
                                  });
}

TEST_CASE("probe: a periodic timer interrupt between ticks wakes its task on time") {
  Probe probe(3);
  probe.world.run_until(milliseconds(30));
  CHECK(probe.firmware_trace() == std::vector<TraceRecord>{
                                      {microseconds(7500), 0, "timer_isr_us", 7500},
                                      {microseconds(15000), 0, "timer_isr_us", 15000},
                                      {microseconds(22500), 0, "timer_isr_us", 22500},
                                  });
}

TEST_CASE("probe: deleting a blocked task, then creating another") {
  Probe probe(4);
  probe.world.run_until(milliseconds(5));
  CHECK(probe.firmware_trace() == std::vector<TraceRecord>{
                                      {milliseconds(1), 0, "deleted", 1},
                                      {milliseconds(1), 0, "replacement_received", 9},
                                      {milliseconds(1), 0, "sent", 9},
                                  });
}

TEST_CASE("probe: a task function that returns fails configASSERT, as in the upstream ports") {
  Probe probe(6);
  probe.world.run_until(milliseconds(10));
  const std::vector<TraceRecord> trace = probe.firmware_trace();
  REQUIRE(trace.size() == 3);
  CHECK(trace[0] == TraceRecord{milliseconds(2), 0, "returning", 1});
  CHECK(trace[1].label == "assert_failed");
  CHECK(trace[2] == TraceRecord{milliseconds(2), 0, "stop", 134});
}

TEST_CASE("probe: a failed configASSERT stops only its machine") {
  World world;
  costar::Machine& failing = world.add_machine(KERNEL_PROBE, {{.name = "probe", .kind = DeviceKind::Registers}});
  failing.set_register("probe", 0, 5);
  world.add_machine(PING_PONG_1);
  world.run_until(milliseconds(20));
  const std::vector<TraceRecord> failed = world.trace().for_machine(0);
  REQUIRE(failed.size() == 3);
  CHECK(failed[1].at == milliseconds(3));
  CHECK(failed[1].label == "assert_failed");
  CHECK(failed[2] == TraceRecord{milliseconds(3), 0, "stop", 134});
  CHECK(world.trace().for_machine(1) == expected_ping_pong(1, 1));
}
