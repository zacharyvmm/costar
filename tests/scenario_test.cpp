#include <doctest/doctest.h>

#include <string>

#include "costar/scenario.hpp"
#include "costar/world.hpp"

using costar::DeviceKind;
using costar::Frame;
using costar::Input;
using costar::milliseconds;
using costar::parse_scenario;
using costar::Scenario;

namespace {

Scenario parse(const std::string& text) { return parse_scenario(text, "test.toml"); }

const char* kTwoBoards = R"(
name = "two boards"
duration_ms = 20

[[machine]]
name = "a"
devices = [
  { name = "uart", kind = "channel", irq = 2 },
  { name = "gpio", kind = "registers" },
  { name = "flash", kind = "memory", size = 16 },
]

[[machine]]
name = "b"
devices = [{ name = "uart", kind = "channel" }]

[[wire]]
endpoints = ["a.uart", "b.uart"]
latency_us = 86.5
bits_per_second = 115200
bits_per_byte = 10
byte_stream = true
faults = [{ kind = "corrupt", from_ms = 1, until_ms = 2 }]

[[input]]
at_ms = 1.5
target = "b.uart"
send_hex = "de:ad be-ef"

[[input]]
target = "a.gpio"
register = 3
value = 7

[[input]]
at_ns = 42
target = "a.flash"
write = "hi"
offset = 4
)";

}  // namespace

TEST_CASE("a scenario file describes machines, wires and timed inputs") {
  const Scenario s = parse(kTwoBoards);
  CHECK(s.name == "two boards");
  CHECK(s.duration == milliseconds(20));

  REQUIRE(s.machines.size() == 2);
  CHECK_FALSE(s.machines[0].image.has_value());
  REQUIRE(s.machines[0].devices.size() == 3);
  CHECK(s.machines[0].devices[0].irq == 2);
  CHECK(s.machines[0].devices[1].kind == DeviceKind::Registers);
  CHECK(s.machines[0].devices[2].size == 16);

  REQUIRE(s.wires.size() == 1);
  const costar::Wire& wire = s.wires[0];
  CHECK(wire.endpoints == std::vector<costar::Endpoint>{{0, "uart"}, {1, "uart"}});
  CHECK(wire.latency == 86'500);
  CHECK(wire.bits_per_byte == 10);
  CHECK(wire.byte_stream);
  CHECK(wire.faults.at(0).from == milliseconds(1));

  REQUIRE(s.inputs.size() == 3);
  CHECK(s.inputs[0].kind == Input::Kind::Send);
  CHECK(s.inputs[0].at == 1'500'000);
  CHECK(s.inputs[0].target == costar::Endpoint{1, "uart"});
  CHECK(s.inputs[0].data == Frame{0xde, 0xad, 0xbe, 0xef});
  CHECK(s.inputs[1].kind == Input::Kind::SetRegister);
  CHECK(s.inputs[1].at == 0);
  CHECK(s.inputs[1].value == 7);
  CHECK(s.inputs[2].kind == Input::Kind::WriteMemory);
  CHECK(s.inputs[2].at == 42);
  CHECK(s.inputs[2].data == Frame{'h', 'i'});
}

TEST_CASE("scenario errors name the file and the problem") {
  CHECK_THROWS_WITH(parse("[[wire]]\nendpoints = [\"x.uart\", \"y.uart\"]"),
                    doctest::Contains("test.toml: no machine named 'x'"));
  CHECK_THROWS_WITH(parse("[[machine]]\nname = \"a\"\n[[input]]\ntarget = \"a.uart\"\nsend = \"x\"\ndeliver = \"y\""),
                    doctest::Contains("exactly one of"));
  CHECK_THROWS_WITH(parse("[[machine]]\nname = \"a\"\nimage = \"missing.so\""),
                    doctest::Contains("image 'missing.so' not found"));
  CHECK_THROWS_WITH(parse("name = "), doctest::Contains("test.toml"));
}

TEST_CASE("build_world runs a scenario's inputs over its wires") {
  const Scenario s = parse(kTwoBoards);
  costar::World world;
  costar::build_world(s, world);
  world.run_until(*s.duration);

  // b sends 4 bytes at 1.5 ms; a UART at 115200 baud delivers them one by one.
  std::vector<costar::Time> arrivals;
  for (const auto& r : world.trace().for_machine(0)) {
    if (r.label == "rx") arrivals.push_back(r.at);
  }
  const costar::Time byte = (10 * 1'000'000'000ULL + 115199) / 115200;  // 86 806 ns
  CHECK(arrivals == std::vector<costar::Time>{1'500'000 + byte + 86'500, 1'500'000 + 2 * byte + 86'500,
                                              1'500'000 + 3 * byte + 86'500, 1'500'000 + 4 * byte + 86'500});
  CHECK(world.machine(0).devices().get<costar::Registers>("gpio").values.at(3) == 7);
  CHECK(world.machine(0).devices().get<costar::Memory>("flash").bytes[4] == 'h');
}
