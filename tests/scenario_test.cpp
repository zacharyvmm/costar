#include <doctest/doctest.h>

#include <filesystem>
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

TEST_CASE("a scenario can set the stack check, for every machine or for one") {
  using costar::StackCheck;
  CHECK_FALSE(parse("name = \"x\"").stack_check.has_value());
  CHECK(parse("stack_check = false").stack_check == StackCheck{.enabled = false});
  CHECK(parse("stack_check = { allowance = 3 }").stack_check == StackCheck{.allowance = 3});
  const Scenario s = parse_scenario(R"(
stack_check = { allowance = 1.5, extra_bytes = 4096 }
[[machine]]
name = "a"
image = "freertos_link_node.so"
stack_check = { extra_bytes = 0 }
[[machine]]
name = "b"
image = "freertos_link_node.so"
stack_check = false
[[machine]]
name = "c"
image = "freertos_link_node.so"
)",
                                    "test.toml", {std::filesystem::path(LINK_NODE).parent_path()});
  CHECK(s.machines[0].stack_check == StackCheck{.allowance = 1.5, .extra = 0});
  CHECK(s.machines[1].stack_check == StackCheck{.enabled = false, .allowance = 1.5, .extra = 4096});
  CHECK_FALSE(s.machines[2].stack_check.has_value());  // the scenario's
  CHECK_THROWS_WITH(parse("stack_check = 2"), doctest::Contains("stack_check must be true, false or a table"));
  CHECK_THROWS_WITH(parse("stack_check = { allowance = 0 }"), doctest::Contains("allowance must be a number above 0"));
  CHECK_THROWS_WITH(parse("stack_check = { allowance = \"2\" }"), doctest::Contains("allowance must be a number"));
  CHECK_THROWS_WITH(parse("stack_check = { extra_bytes = -1 }"), doctest::Contains("extra_bytes must be between"));
  CHECK_THROWS_WITH(parse("stack_check = { slack = 1 }"), doctest::Contains("stack_check: unknown key 'slack'"));
  CHECK_THROWS_WITH(parse("[[machine]]\nname = \"p\"\nstack_check = true"),
                    doctest::Contains("machine 'p': stack_check needs an image"));
}

TEST_CASE("a machine's CPU speed is its time per basic block") {
  const auto machine = [](const std::string& cpu) {
    return "[[machine]]\nname = \"m\"\nimage = \"freertos_link_node.so\"\n" + cpu;
  };
  const auto parse_with_images = [](const std::string& text) {
    return parse_scenario(text, "test.toml", {std::filesystem::path(LINK_NODE).parent_path()});
  };
  CHECK_FALSE(parse_with_images(machine("")).machines[0].block_ps.has_value());
  CHECK(parse_with_images(machine("cpu = { ns_per_block = 12.5 }")).machines[0].block_ps == 12500u);
  CHECK(parse_with_images(machine("cpu = { ns_per_block = 100 }")).machines[0].block_ps == 100000u);
  CHECK_THROWS_WITH(parse_with_images(machine("cpu = { ns_per_block = 0 }")),
                    doctest::Contains("machine 'm': cpu: ns_per_block must be a number from 0.001 to 1000000"));
  CHECK_THROWS_WITH(parse_with_images(machine("cpu = { ns_per_block = \"fast\" }")),
                    doctest::Contains("ns_per_block must be a number"));
  CHECK_THROWS_WITH(parse_with_images(machine("cpu = { }")), doctest::Contains("ns_per_block must be a number"));
  CHECK_THROWS_WITH(parse_with_images(machine("cpu = { mhz = 64 }")), doctest::Contains("cpu: unknown key 'mhz'"));
  CHECK_THROWS_WITH(parse_with_images(machine("cpu = 64")), doctest::Contains("cpu must be a table"));
  CHECK_THROWS_WITH(parse("[[machine]]\nname = \"p\"\ncpu = { ns_per_block = 1 }"),
                    doctest::Contains("machine 'p': cpu needs an image"));
}

TEST_CASE("a watchdog is a registers device") {
  const Scenario s = parse("[[machine]]\nname = \"m\"\ndevices = [{ name = \"wdt\", kind = \"watchdog\" }]\n"
                           "[[input]]\ntarget = \"m.wdt\"\nregister = 3\nvalue = 0");
  CHECK(s.machines[0].devices[0].kind == DeviceKind::Registers);
  CHECK(s.machines[0].devices[0].watchdog);
  CHECK_THROWS_WITH(parse("[[machine]]\nname = \"m\"\ndevices = [{ name = \"wdt\", kind = \"watchdog\", irq = 3 }]"),
                    doctest::Contains("device 'wdt': a watchdog has no irq"));
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
  // 4 GiB of memory is what 32-bit offsets reach, but more than a 32-bit size_t holds.
  const std::string four_gib = "[[machine]]\nname = \"m\"\ndevices = [{ name = \"ram\", kind = \"memory\", size = 4294967296 }]";
  if constexpr (sizeof(size_t) > 4) {
    CHECK(parse(board + four_gib).machines.back().devices.front().size == size_t{1} << 32);
  } else {
    rejects(four_gib, "size must be between 1 and 4294967295");
  }
}

TEST_CASE("bridges: TCP framing and pseudo-terminals") {
  const std::string board = "[[machine]]\nname = \"a\"\ndevices = [{ name = \"uart\", kind = \"channel\" }]\n";
  const Scenario s = parse(board + R"(
[[bridge]]
endpoint = "a.uart"
kind = "tcp-listen"
address = "127.0.0.1:0"
[[bridge]]
endpoint = "a.uart"
kind = "tcp-connect"
address = "127.0.0.1:9"
framing = "raw"
[[bridge]]
endpoint = "a.uart"
kind = "pty"
link = "uart.pty"
[[bridge]]
endpoint = "a.uart"
kind = "pty"
)");
  REQUIRE(s.bridges.size() == 4);
  CHECK(s.bridges[0].framing == costar::Framing::Length);
  CHECK(s.bridges[1].framing == costar::Framing::Raw);
  CHECK(s.bridges[2].kind == costar::BridgeSpec::Kind::Pty);
  CHECK(s.bridges[2].link == "uart.pty");  // next to the scenario
  CHECK(s.bridges[3].link.empty());

  const auto rejects = [&](const std::string& extra, const std::string& message) {
    CHECK_THROWS_WITH(parse(board + "[[bridge]]\nendpoint = \"a.uart\"\n" + extra), doctest::Contains(message));
  };
  rejects("kind = \"tcp-listen\"\naddress = \"127.0.0.1:0\"\nframing = \"slip\"", "framing must be length or raw");
  rejects("kind = \"udp\"\nlocal = \"127.0.0.1:0\"\nframing = \"raw\"", "bridge 1 (udp): unknown key 'framing'");
  rejects("kind = \"pty\"\naddress = \"127.0.0.1:0\"", "bridge 1 (pty): unknown key 'address'");
  rejects("kind = \"serial\"", "kind must be udp, tcp-listen, tcp-connect, tap or pty");
}

TEST_CASE("a golden trace can cover only some machines and labels") {
  const std::string boards = "[[machine]]\nname = \"a\"\n[[machine]]\nname = \"b\"\n";
  const Scenario s = parse(boards + R"(
[expect]
trace = "x.trace"
trace_machines = ["b"]
trace_labels = ["got_*", "tx"]
trace_exclude_labels = ["log"]
)");
  CHECK(s.golden_filter.machines == std::vector<uint32_t>{1});
  CHECK(s.golden_filter.labels == std::vector<std::string>{"got_*", "tx"});
  CHECK(s.golden_filter.excluded_labels == std::vector<std::string>{"log"});
  CHECK(parse(boards + "[expect]\ntrace = \"x.trace\"").golden_filter.empty());

  const auto rejects = [&](const std::string& expect, const std::string& message) {
    CHECK_THROWS_WITH(parse(boards + "[expect]\n" + expect), doctest::Contains(message));
  };
  rejects("trace = \"x\"\ntrace_machines = [\"c\"]", "[expect]: trace_machines: no machine named 'c'");
  rejects("trace = \"x\"\ntrace_labels = \"tx\"", "trace_labels must be an array of strings");
  rejects("trace = \"x\"\ntrace_labels = [1]", "trace_labels must be an array of strings");
  rejects("trace_labels = [\"tx\"]", "trace_machines and trace_labels go with trace");
  rejects("trace = \"x\"\ntrace_label = [\"tx\"]", "unknown key 'trace_label'");
}

TEST_CASE("--set changes a scenario before it is parsed") {
  using costar::Override;
  const auto with = [](std::vector<Override> overrides) {
    return parse_scenario(kTwoBoards, "test.toml", {}, overrides);
  };
  const Scenario s = with({{"duration_ms", "30"},
                           {"name", "renamed"},                       // a bare word is a string
                           {"machine.a.devices.uart.irq", "5"},       // by name
                           {"wire.1.latency_ms", "3"},                // replaces latency_us
                           {"wire.b.uart.bits_per_second", "9600"},   // the wire with this endpoint
                           {"wire.1.faults.1.until_ms", "1.5"},
                           {"input.2.value", "9"},
                           {"wire.1.endpoints.2", "\"b.uart\""}});  // an array's element
  CHECK(s.duration == milliseconds(30));
  CHECK(s.name == "renamed");
  CHECK(s.machines[0].devices[0].irq == 5u);
  CHECK(s.wires[0].latency == milliseconds(3));
  CHECK(s.wires[0].bits_per_second == 9600);
  CHECK(s.wires[0].faults[0].until == 1'500'000);
  CHECK(s.inputs[1].value == 9);
  CHECK(with({{"stall_limit", "7"}, {"stall_limit", "8"}}).stall_limit == 8u);  // in order

  CHECK(costar::parse_override("a.b=c=d") == Override{"a.b", "c=d"});
  CHECK_THROWS_AS(costar::parse_override("novalue"), std::invalid_argument);
  CHECK_THROWS_AS(costar::parse_override("=1"), std::invalid_argument);

  const auto rejects = [&](Override o, const std::string& message) {
    CHECK_THROWS_WITH(with({o}), doctest::Contains(message));
  };
  rejects({"wire.2.latency_ms", "1"}, "--set wire.2.latency_ms: wire has 1 element(s), not 2");
  rejects({"wire.0.latency_ms", "1"}, "wire has 1 element(s), not 0");
  rejects({"machine.c.name", "x"}, "--set machine.c.name: machine has nothing named c");
  rejects({"plant.1.type", "x"}, "--set plant.1.type: the scenario has no plant");
  rejects({"duration_ms.x", "1"}, "duration_ms is a value, not a table");
  rejects({"wire.a.uart", "1"}, "a key must follow a.uart");
  rejects({"wire..latency_ms", "1"}, "a key is names separated by '.'");
  // The result is parsed as strictly as ever.
  rejects({"wire.1.latancy_ms", "1"}, "wire 1: unknown key 'latancy_ms'");
  rejects({"duration_ms", "soon"}, "duration_ms must be a number");
  rejects({"input.1.target", "c.uart"}, "input 1: no machine named 'c'");
}

TEST_CASE("--set reaches stack checks, CPU speeds, watchdogs, models and bridges") {
  using costar::Override;
  const std::string text = R"(
[[machine]]
name = "mcu"
image = "freertos_link_node.so"
devices = [{ name = "i2c", kind = "channel", irq = 3 }, { name = "uart", kind = "channel" }]
cpu = { ns_per_block = 10 }
[[machine]]
name = "board"
devices = [{ name = "i2c", kind = "channel" }, { name = "sensor", kind = "registers" }]
[[wire]]
endpoints = ["mcu.i2c", "board.i2c"]
[[model]]
type = "i2c-registers"
bus = "board.i2c"
address = 0x48
registers = "board.sensor"
[[bridge]]
endpoint = "mcu.uart"
kind = "tcp-listen"
address = "127.0.0.1:0"
)";
  const auto with = [&](std::vector<Override> overrides) {
    return parse_scenario(text, "test.toml", {std::filesystem::path(LINK_NODE).parent_path()}, overrides);
  };
  const Scenario s = with({{"stack_check", "{ allowance = 3.0 }"},  // a table the file doesn't have
                           {"machine.mcu.cpu.ns_per_block", "12.5"},
                           {"machine.board.devices.sensor.kind", "watchdog"},
                           {"model.1.address", "0x49"},
                           {"bridge.1.framing", "raw"}});
  CHECK(s.stack_check == costar::StackCheck{.allowance = 3.0});
  CHECK(s.machines[0].block_ps == 12500u);
  CHECK(s.machines[1].devices[1].watchdog);
  CHECK(std::get<costar::I2cRegistersSpec>(s.models[0].part).address == 0x49);
  CHECK(s.bridges[0].framing == costar::Framing::Raw);
  CHECK(with({{"machine.mcu.stack_check", "false"}}).machines[0].stack_check == costar::StackCheck{.enabled = false});
  // A key below a table the file doesn't have needs the table, whole.
  CHECK_THROWS_WITH(with({{"stack_check.allowance", "3.0"}}), doctest::Contains("the scenario has no stack_check"));
  // And strict parsing still applies to them.
  CHECK_THROWS_WITH(with({{"machine.mcu.cpu.ns_per_block", "0"}}), doctest::Contains("ns_per_block must be a number"));
  CHECK_THROWS_WITH(with({{"model.1.adress", "1"}}), doctest::Contains("unknown key 'adress'"));
  CHECK_THROWS_WITH(with({{"bridge.1.framing", "slip"}}), doctest::Contains("framing must be length or raw"));
}

TEST_CASE("sweeps: values or a range per key, and every combination") {
  const std::string board = R"(
[[machine]]
name = "a"
devices = [{ name = "uart", kind = "channel" }, { name = "seed", kind = "registers" }]
[[machine]]
name = "b"
devices = [{ name = "uart", kind = "channel" }]
[[wire]]
endpoints = ["a.uart", "b.uart"]
[[input]]
target = "a.seed"
register = 0
)";
  const Scenario s = parse(board + R"(
[[sweep]]
key = "wire.1.latency_us"
values = [1, 2.5, 3]
[[sweep]]
key = "input.1.value"
range = [1, 7, 3]
)");
  REQUIRE(s.sweeps.size() == 2);
  CHECK(s.sweeps[0].values == std::vector<std::string>{"1", "2.5", "3"});
  CHECK(s.sweeps[1].values == std::vector<std::string>{"1", "4", "7"});
  const auto combinations = costar::sweep_combinations(s);
  REQUIRE(combinations.size() == 9);
  CHECK(combinations[1] == std::vector<costar::Override>{{"wire.1.latency_us", "1"}, {"input.1.value", "4"}});
  CHECK(combinations[8] == std::vector<costar::Override>{{"wire.1.latency_us", "3"}, {"input.1.value", "7"}});
  CHECK(costar::sweep_combinations(parse(board)).empty());
  // A float range includes its end, give or take rounding.
  CHECK(parse(board + "[[sweep]]\nkey = \"wire.1.latency_ms\"\nrange = [0.1, 0.3, 0.1]").sweeps[0].values ==
        std::vector<std::string>{"0.1", "0.2", "0.3"});
  CHECK(parse(board + "[[sweep]]\nkey = \"wire.1.latency_ms\"\nrange = [1.0, 2, 0.5]").sweeps[0].values ==
        std::vector<std::string>{"1.0", "1.5", "2.0"});

  const auto rejects = [&](const std::string& extra, const std::string& message) {
    CHECK_THROWS_WITH(parse(board + extra), doctest::Contains(message));
  };
  rejects("[[sweep]]\nkey = \"duration_ms\"", "sweep 1: needs one of values and range");
  rejects("[[sweep]]\nkey = \"duration_ms\"\nvalues = []", "values must be a non-empty array");
  rejects("[[sweep]]\nkey = \"duration_ms\"\nvalues = [[1]]", "each value must be a number, string or boolean");
  rejects("[[sweep]]\nkey = \"duration_ms\"\nrange = [5, 1, 1]", "range needs from <= to and a positive step");
  rejects("[[sweep]]\nkey = \"duration_ms\"\nrange = [1, 5]", "range must be [from, to, step]");
  rejects("[[sweep]]\nkey = \"duration_ms\"\nvalues = [1]\nstep = 1", "sweep 1: unknown key 'step'");
  // Each value is checked when the scenario loads.
  rejects("[[sweep]]\nkey = \"wire.2.latency_ms\"\nvalues = [1]",
          "test.toml: sweep 1 (wire.2.latency_ms=1): --set wire.2.latency_ms: wire has 1 element(s), not 2");
  rejects("[[sweep]]\nkey = \"input.1.value\"\nvalues = [1, -1]",
          "sweep 1 (input.1.value=-1): input 1: value must be between 0 and 4294967295");
  CHECK_THROWS_WITH(parse("max_runs = 5\n" + board + "[[sweep]]\nkey = \"input.1.value\"\nrange = [1, 3, 1]\n" +
                          "[[sweep]]\nkey = \"wire.1.latency_ms\"\nvalues = [1, 2]"),
                    doctest::Contains("the sweeps have more combinations than max_runs (5)"));
  CHECK(parse("max_runs = 6\n" + board + "[[sweep]]\nkey = \"input.1.value\"\nrange = [1, 3, 1]\n" +
              "[[sweep]]\nkey = \"wire.1.latency_ms\"\nvalues = [1, 2]").max_runs == 6);
}

TEST_CASE("sweeps: ranges that can't be generated are load-time errors") {
  const std::string plant = "[[plant]]\ntype = \"thermal\"\nperiod_ms = 1\ngain = 1\n";
  const auto range = [&](const std::string& bounds, const std::string& extra = "") {
    return parse(extra + plant + "[[sweep]]\nkey = \"plant.1.gain\"\nrange = " + bounds).sweeps.at(0).values;
  };
  const auto rejects = [&](const std::string& bounds, const std::string& message, const std::string& extra = "") {
    CHECK_THROWS_WITH(range(bounds, extra), doctest::Contains(message));
  };
  // Integer ranges across all of int64_t: distances don't overflow.
  CHECK(range("[-9223372036854775808, 9223372036854775807, 9223372036854775807]") ==
        std::vector<std::string>{"-9223372036854775808", "-1", "9223372036854775806"});
  CHECK(range("[9223372036854775806, 9223372036854775807, 9223372036854775807]") ==
        std::vector<std::string>{"9223372036854775806"});
  // Float ranges need finite bounds and step, and must advance.
  rejects("[0.0, 1.0, inf]", "sweep 1: range needs finite numbers, from <= to and a positive step");
  rejects("[0.0, inf, 1.0]", "range needs finite numbers");
  rejects("[-inf, 1.0, 1.0]", "range needs finite numbers");
  rejects("[0.0, 1.0, nan]", "range needs finite numbers");
  rejects("[1e308, 1e308, 1e-308]", "sweep 1: range does not advance");
  rejects("[1.0, 1.0000000000000002, 1e-17]", "range does not advance");
  CHECK(range("[1e308, 1.7e308, 3.5e307]").size() == 3);
  // A range stops at max_runs values (and at a million in any case), before
  // it is generated in full.
  rejects("[1, 1000000000000, 1]", "sweep 1: range has more values than max_runs (1000)");
  rejects("[0.0, 1e300, 1e-300]", "sweep 1: range has more values than max_runs (5)", "max_runs = 5\n");
  rejects("[1, 1000000000000, 1]", "sweep 1: range has too many values", "max_runs = 1000000000\n");
  CHECK(range("[1, 5, 1]", "max_runs = 5\n").size() == 5);
  rejects("[1, 6, 1]", "range has more values than max_runs (5)", "max_runs = 5\n");
  rejects("[0.5, 3.0, 0.5]", "range has more values than max_runs (5)", "max_runs = 5\n");
  CHECK(range("[0.5, 2.5, 0.5]", "max_runs = 5\n").size() == 5);
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
