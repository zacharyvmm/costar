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
                    doctest::Contains("test.toml: wire 1: no machine named 'x'"));
  CHECK_THROWS_WITH(parse("[[machine]]\nname = \"a\"\n[[input]]\ntarget = \"a.uart\"\nsend = \"x\"\ndeliver = \"y\""),
                    doctest::Contains("exactly one of"));
  CHECK_THROWS_WITH(parse("[[machine]]\nname = \"a\"\nimage = \"missing.so\""),
                    doctest::Contains("image 'missing.so' not found"));
  CHECK_THROWS_WITH(parse("name = "), doctest::Contains("test.toml"));
}

TEST_CASE("a scenario can set the stall limit") {
  CHECK_FALSE(parse("name = \"x\"").stall_limit.has_value());
  CHECK(parse("stall_limit = 0").stall_limit == 0u);
  CHECK(parse("stall_limit = 5000000").stall_limit == 5000000u);
  CHECK_THROWS_WITH(parse("stall_limit = -1"), doctest::Contains("stall_limit must be between 0"));
  CHECK_THROWS_WITH(parse("stall_limit = 1.5"), doctest::Contains("stall_limit must be an integer"));
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

TEST_CASE("scenarios reject values that would silently change behaviour") {
  const std::string board = R"(
[[machine]]
name = "a"
devices = [{ name = "uart", kind = "channel" }, { name = "gpio", kind = "registers" },
           { name = "flash", kind = "memory", size = 8 }]
[[machine]]
name = "b"
devices = [{ name = "uart", kind = "channel" }]
)";
  const auto rejects = [&](const std::string& extra, const std::string& message) {
    CHECK_THROWS_WITH(parse(board + extra), doctest::Contains(message));
  };
  // Types and shapes.
  rejects("[[wire]]\nendpoints = [\"a.uart\", \"b.uart\"]\nfaults = [1]", "each fault must be a table");
  rejects("[[input]]\ntarget = \"a.uart\"\nsend = \"x\"\nat_ms = \"soon\"", "at_ms must be a number");
  rejects("[expect]\ntrace = 123", "trace must be a string");
  rejects("[[machine]]\nname = \"c\"\nimage = 123", "image must be a string");
  rejects("[[wire]]\nendpoints = [\"a.uart\", \"b.uart\"]\nlatancy_ms = 1", "unknown key 'latancy_ms'");
  // Ranges.
  rejects("[[machine]]\nname = \"c\"\ndevices = [{ name = \"u\", kind = \"channel\", irq = 64 }]", "irq must be between 0 and 63");
  rejects("[[input]]\ntarget = \"a.uart\"\nsend = \"x\"\nat_ns = -1", "non-negative time");
  rejects("[[input]]\ntarget = \"a.uart\"\nsend = \"x\"\nat_ms = nan", "non-negative time");
  rejects("[[input]]\ntarget = \"a.uart\"\nsend = \"x\"\nat_ms = 1e30", "non-negative time");
  rejects("[[input]]\ntarget = \"a.gpio\"\nregister = 4294967296", "register must be between");
  rejects("[[input]]\ntarget = \"a.gpio\"\nregister = 1\nvalue = 4294967297", "value must be between");
  rejects("[[input]]\ntarget = \"a.flash\"\nwrite = \"x\"\noffset = 4294967296", "offset must be between");
  rejects("[[wire]]\nendpoints = [\"a.uart\", \"b.uart\"]\nbits_per_byte = 0", "bits_per_byte must be between 1");
  rejects("[[wire]]\nendpoints = [\"a.uart\", \"b.uart\"]\nbits_per_second = -1", "bits_per_second must be between 0");
  // References.
  rejects("[[machine]]\nname = \"a\"", "two machines are named 'a'");
  rejects("[[input]]\ntarget = \"a.gpio\"\nsend = \"x\"", "a.gpio is not a channel device");
  rejects("[[input]]\ntarget = \"a.nothing\"\nsend = \"x\"", "machine 'a' has no device 'nothing'");
  rejects("[[input]]\ntarget = \"a.flash\"\nwrite = \"123456789\"", "write runs past the end of memory");
  rejects("[[wire]]\nendpoints = [\"a.uart\", \"a.gpio\"]", "a.gpio is not a channel device");
  rejects("[[plant]]\ntype = \"thermal\"\nperiod_ms = 0", "needs a positive period");
  // Faults, power and expectations.
  rejects("[[wire]]\nendpoints = [\"a.uart\", \"b.uart\"]\nfaults = [{ kind = \"delay\" }]", "needs a delay");
  rejects("[[wire]]\nendpoints = [\"a.uart\", \"b.uart\"]\nfaults = [{ kind = \"drop\", delay_ms = 1 }]",
          "only delay faults have a delay");
  rejects("[[input]]\ntarget = \"a.uart\"\npower = \"off\"", "target is a machine, not a device");
  rejects("[[input]]\ntarget = \"c\"\npower = \"off\"", "no machine named 'c'");
  rejects("[[input]]\ntarget = \"a\"\npower = \"sleep\"", "power must be off, on or reset");
  rejects("[[expect.event]]\nlabel = \"x\"\nvalue = 1\ntext = \"y\"", "expect.event 1: give only one of value and text");
  rejects("[[expect.no]]\nlabel = \"x\"\nafter_ms = 2\nbefore_ms = 1", "expect.no 1: after is later than before");
  rejects("[[expect.event]]\nmachine = \"c\"\nlabel = \"x\"", "no machine named 'c'");
  rejects("[[expect.event]]\nevent = \"x\"", "unknown key 'event'");
}

TEST_CASE("expectations check what happened, where and when") {
  const Scenario s = parse(R"(
[[machine]]
name = "a"
[[machine]]
name = "b"
[[expect.event]]
label = "x"
value = 1
machine = "b"
before_ms = 5
[[expect.no]]
label = "x"
after_ms = 6
[[expect.event]]
label = "log"
text = ""
)");
  const auto failures = [&](std::vector<costar::TraceRecord> records) {
    costar::Trace trace;
    for (auto& r : records) trace.record(std::move(r));
    return costar::check_expectations(s, trace);
  };
  const costar::TraceRecord blank_line{0, 0, "log", 0, ""};
  CHECK(failures({{milliseconds(5), 1, "x", 1}, blank_line}).empty());
  // A text record's value counts too (a frame's size, say).
  CHECK(failures({{milliseconds(5), 1, "x", 1, "text"}, blank_line}).empty());
  // Too late, wrong machine, wrong value: never happened.
  for (const costar::TraceRecord& wrong : {costar::TraceRecord{milliseconds(5) + 1, 1, "x", 1},
                                           costar::TraceRecord{milliseconds(1), 0, "x", 1},
                                           costar::TraceRecord{milliseconds(1), 1, "x", 2, "text"}}) {
    CHECK(failures({wrong, blank_line}) ==
          std::vector<std::string>{"expect.event 1: expected b x 1 at or before 5000000 ns, but it never happened"});
  }
  CHECK(failures({{milliseconds(1), 1, "x", 1}, blank_line, {milliseconds(6), 0, "x", 9}}) ==
        std::vector<std::string>{"expect.no 1: expected no any machine x at or after 6000000 ns, but got: 6000000 m0 x 9"});
  CHECK(failures({{milliseconds(1), 1, "x", 1}, {0, 0, "log", 0}}) ==
        std::vector<std::string>{"expect.event 2: expected any machine log \"\", but it never happened"});
}
