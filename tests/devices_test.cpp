#include <doctest/doctest.h>

#include <memory>
#include <string>
#include <vector>

#include "costar/world.hpp"

using costar::DeviceKind;
using costar::DeviceSpec;
using costar::Endpoint;
using costar::Frame;
using costar::milliseconds;
using costar::Registers;
using costar::Time;
using costar::TraceRecord;
using costar::Wire;
using costar::WireFault;
using costar::World;

namespace {

constexpr Time kMicrosecond = 1'000;

// Board for firmware/freertos_link_node.
std::vector<DeviceSpec> link_node_board() {
  return {{.name = "link", .kind = DeviceKind::Channel, .irq = 1},
          {.name = "node", .kind = DeviceKind::Registers}};
}

void add_link_node(World& world, uint32_t role) {
  world.add_machine(LINK_NODE, link_node_board()).set_register("node", 0, role);
}

std::vector<TraceRecord> labelled(const World& world, uint32_t machine, const std::string& label) {
  std::vector<TraceRecord> out;
  for (const TraceRecord& r : world.trace().for_machine(machine)) {
    if (r.label == label) out.push_back(r);
  }
  return out;
}

std::vector<Time> times_of(const std::vector<TraceRecord>& records) {
  std::vector<Time> out;
  for (const TraceRecord& r : records) out.push_back(r.at);
  return out;
}

}  // namespace

TEST_CASE("an arriving frame interrupts the CPU and its ISR wakes a task") {
  World world;
  add_link_node(world, 0);  // initiator
  add_link_node(world, 1);  // responder
  world.connect(Wire{.endpoints = {{0, "link"}, {1, "link"}}, .latency = milliseconds(2)});
  world.run_until(milliseconds(50));

  std::vector<TraceRecord> expected{{0, 1, "set", 1, "node[0]=1"}};  // the responder's role
  for (uint32_t n = 1; n <= 3; ++n) {
    const Time sent = milliseconds((n - 1) * 10);
    const std::string ping = "link 500" + std::to_string(n);
    const std::string pong = "link 520" + std::to_string(n);
    expected.push_back({sent, 0, "tx", 2, ping});
    expected.push_back({sent + milliseconds(2), 1, "rx", 2, ping});
    expected.push_back({sent + milliseconds(2), 1, "got_ping", n});
    expected.push_back({sent + milliseconds(2), 1, "tx", 2, pong});
    expected.push_back({sent + milliseconds(4), 0, "rx", 2, pong});
    expected.push_back({sent + milliseconds(4), 0, "got_pong", n});
  }
  CHECK(world.trace().records() == expected);
}

TEST_CASE("a bus carries one frame at a time to every other node") {
  World world;
  add_link_node(world, 0);
  add_link_node(world, 1);
  add_link_node(world, 2);
  world.connect(Wire{.endpoints = {{0, "link"}, {1, "link"}, {2, "link"}},
                     .bits_per_second = 1'000'000,  // a 2-byte frame takes 16 µs
                     .shared_medium = true});
  world.run_until(milliseconds(50));

  // Both responders answer at 16 µs; the second answer waits for the bus.
  std::vector<Time> expected;
  for (Time n = 0; n < 3; ++n) {
    expected.push_back(milliseconds(n * 10) + 32 * kMicrosecond);
    expected.push_back(milliseconds(n * 10) + 48 * kMicrosecond);
  }
  CHECK(times_of(labelled(world, 0, "got_pong")) == expected);
  // Interrupts arrive between ticks, not just on them.
  CHECK(labelled(world, 1, "got_ping").front().at == 16 * kMicrosecond);
}

TEST_CASE("a fault window drops frames on a wire") {
  World world;
  add_link_node(world, 0);
  add_link_node(world, 1);
  world.connect(Wire{.endpoints = {{0, "link"}, {1, "link"}},
                     .latency = milliseconds(2),
                     .faults = {{WireFault::Kind::Drop, milliseconds(5), milliseconds(15)}}});
  world.run_until(milliseconds(50));

  CHECK(times_of(labelled(world, 0, "drop")) == std::vector<Time>{milliseconds(10)});
  CHECK(labelled(world, 1, "got_ping").size() == 2);
  CHECK(labelled(world, 0, "got_pong").size() == 2);
}

namespace {

// A room heated by the thermostat's heater: +0.5 °C per step with the heater
// on, -0.3 °C with it off.
struct Room : costar::Plant {
  uint32_t temperature = 180;
  void step(Time, World& world) override {
    costar::Machine& thermostat = world.machine(0);
    const uint32_t heater = thermostat.devices().get<Registers>("thermo").values[1];
    thermostat.set_register("thermo", 0, temperature);
    temperature = heater ? temperature + 5 : temperature - 3;
  }
};

}  // namespace

TEST_CASE("a plant closes the loop through registers, and firmware logs to memory") {
  World world;
  world.add_machine(THERMOSTAT, {{.name = "thermo", .kind = DeviceKind::Registers},
                                 {.name = "log", .kind = DeviceKind::Memory, .size = 32}});
  world.add_plant(std::make_unique<Room>(), milliseconds(50));
  world.run_until(milliseconds(950));

  // Reference model: plant steps every 50 ms run before the firmware samples
  // (every 100 ms) at the same instant.
  uint32_t temperature = 180, heater = 0;
  std::vector<uint32_t> samples, heater_writes;
  for (Time t = 0; t <= milliseconds(950); t += milliseconds(50)) {
    const uint32_t published = temperature;
    temperature = heater ? temperature + 5 : temperature - 3;
    if (t % milliseconds(100) == 0) {
      samples.push_back(published);
      heater = published < 200;
      heater_writes.push_back(heater);
    }
  }

  std::vector<uint32_t> traced_heater;
  for (const TraceRecord& r : labelled(world, 0, "reg")) traced_heater.push_back(r.value);
  CHECK(traced_heater == heater_writes);

  Frame expected_log;
  for (uint32_t sample : samples) {
    expected_log.push_back(static_cast<uint8_t>(sample));
    expected_log.push_back(static_cast<uint8_t>(sample >> 8));
  }
  expected_log.resize(32, 0xFF);  // the rest of the memory stays erased
  CHECK(world.machine(0).devices().get<costar::Memory>("log").bytes == expected_log);
}

TEST_CASE("devices are looked up by name and shape") {
  costar::Devices devices({{.name = "uart", .kind = DeviceKind::Channel, .irq = 3},
                           {.name = "flash", .kind = DeviceKind::Memory, .size = 4}});
  CHECK(devices.find("uart") == 0);
  CHECK(devices.find("missing") == -1);
  CHECK(devices.get<costar::Channel>(0) != nullptr);
  CHECK(devices.get<costar::Memory>(0) == nullptr);
  CHECK(devices.get<costar::Memory>("flash").bytes == Frame(4, 0xFF));
  CHECK_THROWS(devices.get<Registers>("flash"));
}

TEST_CASE("frames queued between runs are sent") {
  World world;
  world.add_passive_machine({{.name = "link", .kind = DeviceKind::Channel}});
  world.add_passive_machine({{.name = "link", .kind = DeviceKind::Channel}});
  world.connect(Wire{.endpoints = {{0, "link"}, {1, "link"}}, .latency = 5});
  const uint8_t frame[] = {1, 2};
  REQUIRE(world.machine(0).chan_send(0, frame) == 0);
  world.run_until(100);
  CHECK(times_of(labelled(world, 1, "rx")) == std::vector<Time>{5});
}

namespace {
struct CountingPlant : costar::Plant {
  int* steps;
  explicit CountingPlant(int* s) : steps(s) {}
  void step(Time, World&) override { ++*steps; }
};
}  // namespace

TEST_CASE("plants need a positive period and stop at the end of time") {
  int steps = 0;
  World world;
  CHECK_THROWS_AS(world.add_plant(std::make_unique<CountingPlant>(&steps), 0), std::invalid_argument);

  world.run_until(costar::kNever - 10);
  world.add_plant(std::make_unique<CountingPlant>(&steps), 20);
  world.run();
  CHECK(steps == 1);  // the next step would be past the end of time
}

TEST_CASE("idle_until lets time pass but leaves what is due then pending") {
  World world;
  costar::Machine& machine = world.add_passive_machine({{.name = "gpio", .kind = costar::DeviceKind::Registers}});
  world.at(milliseconds(5), [&] { machine.set_register("gpio", 0, 1); });
  world.idle_until(milliseconds(5));
  CHECK(world.now() == milliseconds(5));
  CHECK(world.trace().records().empty());
  CHECK_THROWS_AS(world.idle_until(milliseconds(6)), std::logic_error);
  world.run_until(milliseconds(5));
  CHECK(world.trace().records().size() == 1);
}
