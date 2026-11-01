#include <doctest/doctest.h>

#include <algorithm>
#include <fstream>
#include <iterator>
#include <map>
#include <optional>
#include <string>
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

TEST_CASE("firmware that polls without letting time pass stalls only its machine") {
  World world;
  costar::Machine& poller = world.add_machine(STALL_IMAGE, {{.name = "status", .kind = DeviceKind::Registers}});
  world.add_machine(PING_PONG_1);
  world.at(milliseconds(5), [&] { poller.set_register("status", 0, 1); });  // never seen: time stood still
  world.at(milliseconds(6), [&] {  // a reboot reads the register at once
    poller.power_off();
    poller.power_on();
  });
  world.run_until(milliseconds(20));
  CHECK(world.trace().for_machine(0) == std::vector<TraceRecord>{
                                            {0, 0, "stall", 0},
                                            {milliseconds(5), 0, "set", 1, "status[0]=1"},
                                            {milliseconds(6), 0, "power", 0},
                                            {milliseconds(6), 0, "power", 1},
                                            {milliseconds(6), 0, "ready", 1},
                                        });
  CHECK(world.stalls() == std::vector<costar::Stall>{{0, 0}});
  CHECK(world.trace().for_machine(1) == expected_ping_pong(1, 1));
}

TEST_CASE("the stall limit counts firmware calls at one instant") {
  // The ping-pong makes a few dozen calls per instant: a limit of 10 stops it
  // at boot, 1000 lets it run.
  for (const auto& [limit, stalls] : {std::pair{10, true}, std::pair{1000, false}}) {
    World world;
    world.set_stall_limit(limit);
    world.add_machine(PING_PONG_1);
    world.run_until(milliseconds(20));
    CAPTURE(limit);
    CHECK((world.trace().for_machine(0) == std::vector<TraceRecord>{{0, 0, "stall", 0}}) == stalls);
    CHECK(world.stalls().size() == (stalls ? 1u : 0u));
    if (!stalls) CHECK(world.trace().for_machine(0) == expected_ping_pong(0, 1));
  }
}

// Memory mappings of this process (stacks, fake stacks, images).
static size_t mapping_count() {
  std::ifstream maps("/proc/self/maps");
  size_t count = 0;
  for (std::string line; std::getline(maps, line);) ++count;
  return count;
}

TEST_CASE("power cycles leave no stacks behind") {
  // Discarded threads that had run must give back their stacks and, under
  // AddressSanitizer, their fake stacks.
  World world;
  costar::Machine& machine = world.add_machine(PING_PONG_1);
  const auto cycle = [&](int times) {
    for (int i = 0; i < times; ++i) {
      world.run_until(world.now() + milliseconds(1));
      machine.power_off();
      machine.power_on();
    }
  };
  cycle(20);  // warm up allocator caches
  const size_t before = mapping_count();
  cycle(200);
  CHECK(mapping_count() < before + 20);
}

TEST_CASE("powering a machine off and on boots its firmware from scratch") {
  World world;
  costar::Machine& machine = world.add_machine(PING_PONG_1);
  world.add_machine(PING_PONG_1);  // a neighbour that keeps running
  world.at(microseconds(2500), [&] { machine.power_off(); });
  world.at(milliseconds(4), [&] { machine.power_on(); });
  world.run_until(milliseconds(20));

  const std::vector<TraceRecord> uninterrupted = expected_ping_pong(0, 1);
  std::vector<TraceRecord> expected(uninterrupted.begin(), uninterrupted.begin() + 6);
  expected.push_back({microseconds(2500), 0, "power", 0});
  expected.push_back({milliseconds(4), 0, "power", 1});
  for (TraceRecord r : uninterrupted) {  // fresh globals: it counts from 1 again
    r.at += milliseconds(4);
    expected.push_back(r);
  }
  CHECK(world.trace().for_machine(0) == expected);
  CHECK(world.trace().for_machine(1) == expected_ping_pong(1, 1));
}

TEST_CASE("a machine survives being power cycled, repeatedly") {
  World world;
  costar::Machine& machine = world.add_machine(PING_PONG_3);
  for (uint64_t i = 0; i < 50; ++i) {
    world.at(milliseconds(i) + microseconds(500), [&] { machine.power_off(); });
    world.at(milliseconds(i) + microseconds(700), [&] { machine.power_on(); });
  }
  world.run_until(milliseconds(100));
  // After the last boot (between ticks, so the times differ), a whole run.
  const auto what = [](const TraceRecord& r) { return std::pair{r.label, r.value}; };
  std::vector<std::pair<std::string, uint32_t>> tail;
  for (const TraceRecord& r : world.trace().for_machine(0)) {
    if (r.at >= milliseconds(49) + microseconds(700) && r.label != "power") tail.push_back(what(r));
  }
  std::vector<std::pair<std::string, uint32_t>> expected;
  std::ranges::transform(expected_ping_pong(0, 3), std::back_inserter(expected), what);
  CHECK(tail == expected);
}

// ── Thread stacks (firmware/freertos_stack_overflow) ────────────────────

namespace {

// A machine whose "parser" task (256 words: 1 KiB on the target) copies a
// message of `length` bytes onto its stack; `jump` leaves the copy out and
// uses the stack below it instead.
costar::Machine& add_parser(World& world, uint32_t length, bool jump = false) {
  costar::Machine& machine = world.add_machine(STACK_IMAGE, {{.name = "message", .kind = DeviceKind::Registers}});
  machine.set_register("message", 0, length);
  machine.set_register("message", 1, jump ? 1 : 0);
  return machine;
}

std::vector<std::string> labels(const World& world) {
  std::vector<std::string> out;
  for (const TraceRecord& r : world.trace().records()) {
    if (r.label != "set") out.push_back(r.label);
  }
  return out;
}

}  // namespace

TEST_CASE("a task that overruns its stack stops its machine when it switches away") {
  World world;
  add_parser(world, 32 * 1024);
  world.add_machine(PING_PONG_1);  // a neighbour that keeps running
  world.run_until(milliseconds(20));

  const std::vector<costar::StackOverrun> overruns = world.stack_overruns();
  REQUIRE(overruns.size() == 1);
  CHECK(overruns[0].at == 0);
  CHECK(overruns[0].machine == 0);
  CHECK(overruns[0].thread.name == "parser");
  CHECK(overruns[0].thread.declared == 1024);
  CHECK(overruns[0].thread.limit == 2 * 1024 + 8 * 1024);  // the default: twice the size, plus 8 KiB
  CHECK(overruns[0].thread.used > 32 * 1024);
  // It stopped at its next costar call, which still traced.
  const std::vector<TraceRecord> parser = world.trace().for_machine(0);
  REQUIRE(parser.size() == 3);
  CHECK(parser[1].label == "checksum");
  CHECK(parser[2].label == "stack");
  CHECK(parser[2].text.value().starts_with("parser used "));
  CHECK(world.machine(0).cpu().state() == costar::Cpu::State::Exited);
  CHECK(world.trace().for_machine(1) == expected_ping_pong(1, 1));
}

TEST_CASE("the stack check can be tightened, loosened or switched off") {
  const auto overran = [](uint32_t length, std::optional<costar::StackCheck> check) {
    World world;
    if (check) world.set_stack_check(*check);
    add_parser(world, length);
    world.run_until(milliseconds(5));
    const bool overran = world.stack_overruns().size() == 1;
    CHECK(std::ranges::count(labels(world), "parsed") == (overran ? 0 : 1));
    return overran;
  };
  CHECK_FALSE(overran(4096, std::nullopt));
  CHECK(overran(4096, costar::StackCheck{.allowance = 1, .extra = 1024}));
  CHECK_FALSE(overran(32 * 1024, costar::StackCheck{.enabled = false}));
  CHECK_FALSE(overran(32 * 1024, costar::StackCheck{.allowance = 2, .extra = 48 * 1024}));
}

TEST_CASE("a task that jumps over its canary is caught when its stack is freed") {
  // The task leaves its buffer untouched, so the canary inside it survives
  // every switch; the idle task frees the deleted task, scanning its stack.
  World world;
  add_parser(world, 32 * 1024, true);
  world.run_until(milliseconds(5));
  CHECK(labels(world) == std::vector<std::string>{"checksum", "parsed", "stack"});
  const std::vector<costar::StackOverrun> overruns = world.stack_overruns();
  REQUIRE(overruns.size() == 1);
  CHECK(overruns[0].thread.name == "parser");
}

TEST_CASE("stacks are measured, and scanned when a machine powers off") {
  World world;
  costar::Machine& machine = add_parser(world, 0);
  world.run_until(milliseconds(5));
  // The parser is gone; the idle and timer tasks, and the boot thread, remain.
  std::vector<std::string> names;
  for (const costar::ThreadStack& thread : machine.cpu().stacks()) {
    names.push_back(thread.name);
    CHECK(thread.used > 0);
    CHECK(thread.used < 64 * 1024);
    CHECK(thread.declared == (thread.name == "thread 0" ? 0u : 1024u));
    CHECK(thread.limit == (thread.declared == 0 ? 0u : 10u * 1024));
  }
  CHECK(names == std::vector<std::string>{"thread 0", "IDLE", "Tmr Svc"});
  machine.power_off();
  CHECK(machine.cpu().stacks().empty());
  CHECK(world.stack_overruns().empty());
}

// ── CPU time (firmware/freertos_deadline, INSTRUMENT_KERNEL) ────────────

namespace {

// When each of the control task's periods ended, and whether it was late.
std::vector<TraceRecord> deadlines(const char* image, std::optional<uint64_t> block_ps) {
  World world;
  costar::Machine& machine = world.add_machine(image);
  if (block_ps) machine.set_block_ps(*block_ps);
  world.run_until(milliseconds(60));
  return world.trace().records();
}

}  // namespace

TEST_CASE("instrumented code takes time at its machine's speed") {
  const std::vector<TraceRecord> normal = deadlines(DEADLINE_IMAGE, std::nullopt);
  REQUIRE(normal.size() == 5);
  for (const TraceRecord& r : normal) CHECK(r.label == "deadline_met");
  CHECK(deadlines(DEADLINE_IMAGE, costar::kDefaultBlockPs) == normal);

  // Twice as fast: the first period's work takes half the time.  (Only the
  // application instrumented: the tick ISR, at 1 ms, would add blocks to one
  // and not the other.)
  const std::vector<TraceRecord> app = deadlines(DEADLINE_APP_IMAGE, std::nullopt);
  const std::vector<TraceRecord> fast = deadlines(DEADLINE_APP_IMAGE, costar::kDefaultBlockPs / 2);
  REQUIRE(app.size() == 5);
  REQUIRE(fast.size() == 5);
  CHECK(fast[0].at * 2 == app[0].at);

  // Ten times slower: every deadline is missed.
  const std::vector<TraceRecord> slow = deadlines(DEADLINE_IMAGE, costar::kDefaultBlockPs * 10);
  REQUIRE_FALSE(slow.empty());
  for (const TraceRecord& r : slow) CHECK(r.label == "deadline_missed");

  CHECK_THROWS(World().add_machine(DEADLINE_IMAGE).set_block_ps(0));
}

TEST_CASE("INSTRUMENT_KERNEL charges the kernel's work too") {
  // The same application, with and without the kernel instrumented: the
  // kernel's blocks (creating the task, the tick ISR, delays) add time.
  const std::vector<TraceRecord> app = deadlines(DEADLINE_APP_IMAGE, std::nullopt);
  const std::vector<TraceRecord> kernel = deadlines(DEADLINE_IMAGE, std::nullopt);
  REQUIRE(app.size() == 5);
  REQUIRE(kernel.size() == 5);
  CHECK(app[0].at < kernel[0].at);
}

// ── Watchdogs (firmware/freertos_watchdog) ──────────────────────────────

namespace {

// A machine whose worker task feeds a watchdog every 25 ms (timeout 50 ms),
// until `control` is set at 60 ms: 1 deadlocks the task, 2 fails an assertion.
costar::Machine& add_watchdog_machine(World& world, uint32_t control) {
  costar::Machine& machine =
      world.add_machine(WATCHDOG_IMAGE, {{.name = "wdt", .kind = DeviceKind::Registers, .watchdog = true},
                                         {.name = "control", .kind = DeviceKind::Registers}});
  if (control != 0) world.at(milliseconds(60), [&machine, control] { machine.set_register("control", 0, control); });
  return machine;
}

// The machine's records, without its register writes (feeds).
std::vector<TraceRecord> without_writes(const World& world, uint32_t machine) {
  std::vector<TraceRecord> out;
  for (const TraceRecord& r : world.trace().for_machine(machine)) {
    if (r.label != "reg") out.push_back(r);
  }
  return out;
}

}  // namespace

TEST_CASE("a watchdog that isn't fed reboots its machine, and firmware sees why") {
  World world;
  costar::Machine& machine = add_watchdog_machine(world, 1);
  world.add_machine(PING_PONG_1);  // a neighbour that keeps running
  world.run_until(milliseconds(200));
  CHECK(without_writes(world, 0) == std::vector<TraceRecord>{
                                        {0, 0, "boot", 0},
                                        {milliseconds(60), 0, "set", 1, "control[0]=1"},
                                        {milliseconds(75), 0, "deadlocked", 0},
                                        {milliseconds(100), 0, "watchdog", 1, "wdt"},
                                        {milliseconds(100), 0, "boot", 1},  // safe mode: fed from then on
                                    });
  const std::map<uint32_t, uint32_t>& wdt = machine.devices().get<costar::Registers>("wdt").values;
  CHECK(wdt.at(COSTAR_WDT_RESETS) == 1);
  CHECK(wdt.at(COSTAR_WDT_CTRL) == 1);  // started again by the new boot
  CHECK(world.trace().for_machine(1) == expected_ping_pong(1, 1));
}

TEST_CASE("a watchdog reboots firmware that stopped itself") {
  World world;
  add_watchdog_machine(world, 2);
  world.run_until(milliseconds(200));
  const std::vector<TraceRecord> records = without_writes(world, 0);
  REQUIRE(records.size() == 6);
  CHECK(records[2].label == "assert_failed");
  CHECK(records[3] == TraceRecord{milliseconds(75), 0, "stop", 134});
  CHECK(records[4] == TraceRecord{milliseconds(100), 0, "watchdog", 1, "wdt"});
  CHECK(records[5] == TraceRecord{milliseconds(100), 0, "boot", 1});
}

TEST_CASE("powering off stops a watchdog, and the host's writes don't start one") {
  World world;
  costar::Machine& machine = add_watchdog_machine(world, 0);
  world.at(milliseconds(10), [&] { machine.power_off(); });
  world.run_until(milliseconds(200));
  CHECK(std::ranges::count(labels(world), "watchdog") == 0);
  CHECK(machine.devices().get<costar::Registers>("wdt").values.at(COSTAR_WDT_CTRL) == 0);

  // A passive machine's watchdog only ever changes when the host writes it.
  World passive;
  costar::Machine& board =
      passive.add_passive_machine({{.name = "wdt", .kind = DeviceKind::Registers, .watchdog = true}});
  board.set_register("wdt", COSTAR_WDT_CTRL, 1);
  passive.run();
  CHECK(passive.quiescent());

  CHECK_THROWS(costar::Devices({{.name = "wdt", .kind = DeviceKind::Channel, .watchdog = true}}));
}
