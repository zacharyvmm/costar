#include "costar/scenario.hpp"

#include <algorithm>
#include <cctype>
#include <cmath>
#include <limits>
#include <fstream>
#include <iterator>
#include <sstream>
#include <stdexcept>

#include <toml++/toml.hpp>

#include "costar.h"
#include "costar/world.hpp"

namespace costar {

namespace fs = std::filesystem;

namespace {

// Reads one scenario file, strictly: unknown keys, wrong types and values
// outside their range are errors that name the file and the table.
// Machines are parsed first so that wires and inputs can refer to them.
class Parser {
 public:
  Parser(const fs::path& source, const std::vector<fs::path>& image_dirs)
      : source_(source), dir_(source.parent_path()), image_dirs_(image_dirs) {}

  Scenario parse(std::string_view text) {
    toml::table root;
    try {
      root = toml::parse(text, source_.string());
    } catch (const toml::parse_error& error) {
      fail(std::string(error.description()));
    }

    const std::string top = "the scenario";
    allow_keys(root, top, {"name", "duration_ms", "duration_us", "duration_ns", "machine", "wire", "input", "plant",
                           "bridge", "expect", "stall_limit"});
    scenario_.name = optional_string(root, "name", top).value_or(source_.stem().string());
    scenario_.duration = time(root, "duration", top);
    if (const auto limit = optional_integer(root, "stall_limit", top, 0, std::numeric_limits<int64_t>::max())) {
      scenario_.stall_limit = static_cast<uint64_t>(*limit);
    }
    for_each_table(root, "machine", [&](const toml::table& t) { scenario_.machines.push_back(machine(t)); });
    for_each_table(root, "wire", [&](const toml::table& t) { scenario_.wires.push_back(wire(t)); });
    for_each_table(root, "input", [&](const toml::table& t) { scenario_.inputs.push_back(input(t)); });
    for_each_table(root, "plant", [&](const toml::table& t) { scenario_.plants.push_back(plant(t)); });
    for_each_table(root, "bridge", [&](const toml::table& t) { scenario_.bridges.push_back(bridge(t)); });
    if (const toml::node* expect = root.get("expect")) {
      if (!expect->is_table()) fail("[expect] must be a table");
      const toml::table& t = *expect->as_table();
      allow_keys(t, "[expect]", {"trace", "event", "no"});
      if (const auto trace = optional_string(t, "trace", "[expect]")) scenario_.expected_trace = dir_ / *trace;
      for (const bool present : {true, false}) {
        size_t index = 0;
        for_each_table(t, present ? "event" : "no", [&](const toml::table& e) {
          scenario_.expectations.push_back(expectation(e, present, ++index));
        });
      }
    }
    return std::move(scenario_);
  }

 private:
  [[noreturn]] void fail(const std::string& what) const { throw std::runtime_error(source_.string() + ": " + what); }

  void allow_keys(const toml::table& t, const std::string& what, std::initializer_list<std::string_view> keys) const {
    for (const auto& [key, node] : t) {
      if (std::ranges::find(keys, key.str()) == keys.end()) {
        fail(what + ": unknown key '" + std::string(key.str()) + "'");
      }
    }
  }

  template <class F>
  void for_each_table(const toml::table& root, std::string_view key, F&& f) const {
    const toml::node* node = root.get(key);
    if (node == nullptr) return;
    const toml::array* array = node->as_array();
    if (array == nullptr) fail(std::string(key) + " must be an array of tables ([[" + std::string(key) + "]])");
    for (const toml::node& element : *array) {
      if (!element.is_table()) fail("each " + std::string(key) + " must be a table");
      f(*element.as_table());
    }
  }

  std::optional<std::string> optional_string(const toml::table& t, std::string_view key, const std::string& what) const {
    const toml::node* node = t.get(key);
    if (node == nullptr) return std::nullopt;
    if (!node->is_string()) fail(what + ": " + std::string(key) + " must be a string");
    return node->value<std::string>();
  }

  std::string required_string(const toml::table& t, std::string_view key, const std::string& what) const {
    const auto value = optional_string(t, key, what);
    if (!value) fail(what + " needs a string '" + std::string(key) + "'");
    return *value;
  }

  std::optional<int64_t> optional_integer(const toml::table& t, std::string_view key, const std::string& what,
                                          int64_t min, int64_t max) const {
    const toml::node* node = t.get(key);
    if (node == nullptr) return std::nullopt;
    if (!node->is_integer()) fail(what + ": " + std::string(key) + " must be an integer");
    const int64_t value = *node->value<int64_t>();
    if (value < min || value > max) {
      fail(what + ": " + std::string(key) + " must be between " + std::to_string(min) + " and " + std::to_string(max));
    }
    return value;
  }

  bool optional_bool(const toml::table& t, std::string_view key, const std::string& what) const {
    const toml::node* node = t.get(key);
    if (node == nullptr) return false;
    if (!node->is_boolean()) fail(what + ": " + std::string(key) + " must be true or false");
    return *node->value<bool>();
  }

  // `<key>_ms`, `<key>_us` or `<key>_ns` (at most one), non-negative and
  // representable.
  std::optional<Time> time(const toml::table& t, std::string_view key, const std::string& what) const {
    std::optional<Time> result;
    for (const auto& [suffix, scale] : {std::pair{"_ms", 1e6L}, std::pair{"_us", 1e3L}, std::pair{"_ns", 1.0L}}) {
      const std::string name = std::string(key) + suffix;
      const toml::node* node = t.get(name);
      if (node == nullptr) continue;
      if (result) fail(what + ": give only one of " + std::string(key) + "_ms, _us or _ns");
      long double value;
      if (node->is_integer()) {
        value = static_cast<long double>(*node->value<int64_t>());
      } else if (node->is_floating_point() && scale != 1.0L) {
        value = *node->value<double>();
      } else {
        fail(what + ": " + name + " must be a number" + (scale == 1.0L ? " of whole nanoseconds" : ""));
      }
      const long double ns = std::round(value * scale);
      if (!std::isfinite(value) || value < 0 || ns >= static_cast<long double>(kNever)) {
        fail(what + ": " + name + " must be a non-negative time that fits in 64-bit nanoseconds");
      }
      result = static_cast<Time>(ns);
    }
    return result;
  }

  // `<key> = "text"` or `<key>_hex = "0a 0b"` (at most one).
  std::optional<Frame> bytes(const toml::table& t, std::string_view key, const std::string& what) const {
    const std::string name(key);
    const auto text = optional_string(t, name, what);
    const auto hex = optional_string(t, name + "_hex", what);
    if (text && hex) fail(what + ": give only one of " + name + " and " + name + "_hex");
    if (text) return Frame(text->begin(), text->end());
    if (!hex) return std::nullopt;
    std::string digits;
    for (char c : *hex) {
      if (c == ' ' || c == ':' || c == '-') continue;
      if (!std::isxdigit(static_cast<unsigned char>(c))) fail(what + ": bad hex digit in " + name + "_hex");
      digits += c;
    }
    if (digits.size() % 2 != 0) fail(what + ": " + name + "_hex needs an even number of digits");
    Frame out;
    for (size_t i = 0; i < digits.size(); i += 2) {
      out.push_back(static_cast<uint8_t>(std::stoul(digits.substr(i, 2), nullptr, 16)));
    }
    return out;
  }

  uint32_t machine_id(const std::string& name, const std::string& what) const {
    for (uint32_t id = 0; id < scenario_.machines.size(); ++id) {
      if (scenario_.machines[id].name == name) return id;
    }
    fail(what + ": no machine named '" + name + "'");
  }

  // "machine.device": an existing device of the given kind.
  Endpoint endpoint(const std::string& text, DeviceKind kind, const std::string& what) const {
    const size_t dot = text.find('.');
    if (dot == std::string::npos) fail(what + ": '" + text + "' should be machine.device");
    const std::string machine = text.substr(0, dot);
    const std::string device = text.substr(dot + 1);
    for (uint32_t id = 0; id < scenario_.machines.size(); ++id) {
      if (scenario_.machines[id].name != machine) continue;
      for (const DeviceSpec& spec : scenario_.machines[id].devices) {
        if (spec.name != device) continue;
        if (spec.kind != kind) fail(what + ": " + text + " is not a " + kind_name(kind) + " device");
        return {id, device};
      }
      fail(what + ": machine '" + machine + "' has no device '" + device + "'");
    }
    fail(what + ": no machine named '" + machine + "'");
  }

  const DeviceSpec& device_of(const Endpoint& endpoint) const {
    for (const DeviceSpec& spec : scenario_.machines[endpoint.machine].devices) {
      if (spec.name == endpoint.device) return spec;
    }
    throw std::logic_error("endpoint() returned an unknown device");
  }

  static std::string kind_name(DeviceKind kind) {
    switch (kind) {
      case DeviceKind::Channel: return "channel";
      case DeviceKind::Registers: return "registers";
      case DeviceKind::Memory: return "memory";
    }
    return "?";
  }

  fs::path find_image(const std::string& image, const std::string& what) const {
    std::vector<fs::path> candidates{dir_ / image};
    for (const fs::path& dir : image_dirs_) candidates.push_back(dir / image);
    for (const fs::path& candidate : candidates) {
      if (fs::exists(candidate)) return candidate;
    }
    fail(what + ": image '" + image + "' not found next to the scenario or in the image directories");
  }

  MachineSpec machine(const toml::table& t) const {
    MachineSpec spec{.name = required_string(t, "name", "a machine")};
    const std::string what = "machine '" + spec.name + "'";
    allow_keys(t, what, {"name", "image", "devices"});
    for (const MachineSpec& other : scenario_.machines) {
      if (other.name == spec.name) fail("two machines are named '" + spec.name + "'");
    }
    if (const auto image = optional_string(t, "image", what)) spec.image = find_image(*image, what);
    if (const toml::node* devices = t.get("devices")) {
      if (!devices->is_array()) fail(what + ": devices must be an array of tables");
      for (const toml::node& node : *devices->as_array()) {
        if (!node.is_table()) fail(what + ": each device must be a table");
        spec.devices.push_back(device(*node.as_table(), what));
        for (size_t i = 0; i + 1 < spec.devices.size(); ++i) {
          if (spec.devices[i].name == spec.devices.back().name) {
            fail(what + ": two devices are named '" + spec.devices.back().name + "'");
          }
        }
      }
    }
    return spec;
  }

  DeviceSpec device(const toml::table& t, const std::string& machine) const {
    DeviceSpec spec{.name = required_string(t, "name", machine + ": a device"), .kind = DeviceKind::Channel};
    const std::string what = machine + ", device '" + spec.name + "'";
    allow_keys(t, what, {"name", "kind", "irq", "size"});
    const std::string kind = required_string(t, "kind", what);
    if (kind == "registers") {
      spec.kind = DeviceKind::Registers;
    } else if (kind == "memory") {
      spec.kind = DeviceKind::Memory;
      const auto size = optional_integer(t, "size", what, 1, int64_t{1} << 32);
      if (!size) fail(what + ": a memory device needs a size");
      spec.size = static_cast<size_t>(*size);
    } else if (kind != "channel") {
      fail(what + ": kind must be channel, registers or memory");
    }
    if (spec.kind != DeviceKind::Memory && t.contains("size")) fail(what + ": only memory devices have a size");
    if (const auto irq = optional_integer(t, "irq", what, 0, COSTAR_IRQ_LINES - 1)) {
      spec.irq = static_cast<unsigned>(*irq);
    }
    return spec;
  }

  Wire wire(const toml::table& t) const {
    const std::string what = "wire " + std::to_string(scenario_.wires.size() + 1);
    allow_keys(t, what, {"endpoints", "latency_ms", "latency_us", "latency_ns", "bits_per_second", "bits_per_byte",
                         "byte_stream", "shared_medium", "faults"});
    Wire w;
    const toml::node* endpoints = t.get("endpoints");
    if (endpoints == nullptr || !endpoints->is_array() || endpoints->as_array()->size() < 2) {
      fail(what + ": endpoints must list at least two \"machine.device\" channels");
    }
    for (const toml::node& node : *endpoints->as_array()) {
      if (!node.is_string()) fail(what + ": endpoints are \"machine.device\" strings");
      w.endpoints.push_back(endpoint(*node.value<std::string>(), DeviceKind::Channel, what));
    }
    w.latency = time(t, "latency", what).value_or(0);
    w.bits_per_second = static_cast<uint64_t>(
        optional_integer(t, "bits_per_second", what, 0, std::numeric_limits<int64_t>::max()).value_or(0));
    w.bits_per_byte = static_cast<uint32_t>(optional_integer(t, "bits_per_byte", what, 1, UINT32_MAX).value_or(8));
    w.byte_stream = optional_bool(t, "byte_stream", what);
    w.shared_medium = optional_bool(t, "shared_medium", what);
    if (const toml::node* faults = t.get("faults")) {
      if (!faults->is_array()) fail(what + ": faults must be an array of tables");
      for (const toml::node& node : *faults->as_array()) {
        if (!node.is_table()) fail(what + ": each fault must be a table");
        const toml::table& f = *node.as_table();
        const std::string fault = what + ", fault";
        allow_keys(f, fault, {"kind", "from_ms", "from_us", "from_ns", "until_ms", "until_us", "until_ns", "delay_ms",
                              "delay_us", "delay_ns", "match", "match_hex"});
        const std::string kind = required_string(f, "kind", fault);
        WireFault parsed{.kind = WireFault::Kind::Drop,
                         .from = time(f, "from", fault).value_or(0),
                         .until = time(f, "until", fault).value_or(kNever),
                         .match = bytes(f, "match", fault).value_or(Frame{})};
        const std::optional<Time> delay = time(f, "delay", fault);
        if (kind == "corrupt") {
          parsed.kind = WireFault::Kind::Corrupt;
        } else if (kind == "delay") {
          if (!delay) fail(fault + ": a delay fault needs a delay");
          parsed.kind = WireFault::Kind::Delay;
          parsed.delay = *delay;
        } else if (kind != "drop") {
          fail(fault + ": kind must be drop, corrupt or delay");
        }
        if (delay && kind != "delay") fail(fault + ": only delay faults have a delay");
        w.faults.push_back(std::move(parsed));
      }
    }
    return w;
  }

  Input input(const toml::table& t) const {
    const std::string what = "input " + std::to_string(scenario_.inputs.size() + 1);
    allow_keys(t, what, {"at_ms", "at_us", "at_ns", "target", "deliver", "deliver_hex", "send", "send_hex", "write",
                         "write_hex", "offset", "register", "value", "power"});
    const std::string target = required_string(t, "target", what);
    Input in{.kind = Input::Kind::Deliver, .at = time(t, "at", what).value_or(0), .target = {}};
    int kinds = 0;
    if (auto data = bytes(t, "deliver", what)) {
      in.kind = Input::Kind::Deliver, in.data = *data, ++kinds;
    }
    if (auto data = bytes(t, "send", what)) {
      in.kind = Input::Kind::Send, in.data = *data, ++kinds;
    }
    if (auto data = bytes(t, "write", what)) {
      in.kind = Input::Kind::WriteMemory, in.data = *data, ++kinds;
    }
    if (t.contains("register")) {
      in.kind = Input::Kind::SetRegister, ++kinds;
      in.reg = static_cast<uint32_t>(*optional_integer(t, "register", what, 0, UINT32_MAX));
      in.value = static_cast<uint32_t>(optional_integer(t, "value", what, 0, UINT32_MAX).value_or(0));
    } else if (t.contains("value")) {
      fail(what + ": value goes with register");
    }
    if (const auto power = optional_string(t, "power", what)) {
      in.kind = Input::Kind::Power, ++kinds;
      if (*power == "off") {
        in.power = Input::Power::Off;
      } else if (*power == "on") {
        in.power = Input::Power::On;
      } else if (*power != "reset") {
        fail(what + ": power must be off, on or reset");
      }
    }
    if (kinds != 1) fail(what + ": needs exactly one of deliver, send, write, register or power");
    if (in.kind == Input::Kind::Power) {
      if (target.find('.') != std::string::npos) fail(what + ": a power input's target is a machine, not a device");
      if (t.contains("offset")) fail(what + ": offset goes with write");
      in.target = {machine_id(target, what), ""};
      return in;
    }

    const DeviceKind kind = in.kind == Input::Kind::SetRegister   ? DeviceKind::Registers
                            : in.kind == Input::Kind::WriteMemory ? DeviceKind::Memory
                                                                  : DeviceKind::Channel;
    in.target = endpoint(target, kind, what);
    if (in.kind == Input::Kind::WriteMemory) {
      in.offset = static_cast<uint32_t>(optional_integer(t, "offset", what, 0, UINT32_MAX).value_or(0));
      if (in.offset + in.data.size() > device_of(in.target).size) fail(what + ": write runs past the end of memory");
    } else if (t.contains("offset")) {
      fail(what + ": offset goes with write");
    }
    return in;
  }

  BridgeSpec bridge(const toml::table& t) const {
    const std::string what = "bridge " + std::to_string(scenario_.bridges.size() + 1);
    allow_keys(t, what, {"endpoint", "kind", "local", "remote", "address", "interface"});
    BridgeSpec spec{.endpoint = endpoint(required_string(t, "endpoint", what), DeviceKind::Channel, what),
                    .kind = BridgeSpec::Kind::Udp};
    const std::string kind = required_string(t, "kind", what);
    const auto only = [&](std::initializer_list<std::string_view> keys) {
      allow_keys(t, what + " (" + kind + ")", keys);
    };
    if (kind == "udp") {
      only({"endpoint", "kind", "local", "remote"});
      spec.local = required_string(t, "local", what);
      spec.remote = optional_string(t, "remote", what).value_or("");
    } else if (kind == "tcp-listen" || kind == "tcp-connect") {
      only({"endpoint", "kind", "address"});
      spec.kind = kind == "tcp-listen" ? BridgeSpec::Kind::TcpListen : BridgeSpec::Kind::TcpConnect;
      spec.address = required_string(t, "address", what);
    } else if (kind == "tap") {
      only({"endpoint", "kind", "interface"});
      spec.kind = BridgeSpec::Kind::Tap;
      spec.interface = required_string(t, "interface", what);
    } else {
      fail(what + ": kind must be udp, tcp-listen, tcp-connect or tap");
    }
    return spec;
  }

  Expectation expectation(const toml::table& t, bool present, size_t index) const {
    const std::string what = (present ? "expect.event " : "expect.no ") + std::to_string(index);
    allow_keys(t, what, {"label", "machine", "value", "text", "after_ms", "after_us", "after_ns", "before_ms",
                         "before_us", "before_ns"});
    Expectation e{.present = present, .label = required_string(t, "label", what), .name = what};
    if (const auto machine = optional_string(t, "machine", what)) e.machine = machine_id(*machine, what);
    if (const auto value = optional_integer(t, "value", what, 0, UINT32_MAX)) e.value = static_cast<uint32_t>(*value);
    e.text = optional_string(t, "text", what);
    if (e.value && e.text) fail(what + ": give only one of value and text");
    e.after = time(t, "after", what).value_or(0);
    e.before = time(t, "before", what).value_or(kNever);
    if (e.after > e.before) fail(what + ": after is later than before");
    return e;
  }

  PlantSpec plant(const toml::table& t) const {
    const std::string type = required_string(t, "type", "a plant");
    const std::string what = "plant '" + type + "'";
    const std::optional<Time> period = time(t, "period", what);
    if (!period || *period == 0) fail(what + " needs a positive period");
    PlantSpec spec{.type = type, .period = *period};
    for (const auto& [key, node] : t) {
      const std::string name(key.str());
      if (name == "type" || name == "period_ms" || name == "period_us" || name == "period_ns") continue;
      if (const auto i = node.value_exact<int64_t>()) {
        spec.params[name] = *i;
      } else if (const auto d = node.value_exact<double>()) {
        spec.params[name] = *d;
      } else if (const auto s = node.value_exact<std::string>()) {
        spec.params[name] = *s;
      } else {
        fail(what + ": parameter " + name + " must be a number or a string");
      }
    }
    return spec;
  }

  fs::path source_;
  fs::path dir_;
  std::vector<fs::path> image_dirs_;
  Scenario scenario_;
};

void apply(Machine& machine, const Input& input) {
  const std::string& device = input.target.device;
  switch (input.kind) {
    case Input::Kind::Deliver: machine.deliver(device, input.data); break;
    case Input::Kind::Send: machine.send(device, input.data); break;
    case Input::Kind::SetRegister: machine.set_register(device, input.reg, input.value); break;
    case Input::Kind::WriteMemory: machine.write_memory(device, input.offset, input.data); break;
    case Input::Kind::Power:
      if (input.power != Input::Power::On) machine.power_off();
      if (input.power != Input::Power::Off) machine.power_on();
      break;
  }
}

// "m0 label value=3 at or before 15000000 ns", for messages.
std::string describe(const Expectation& e, const Scenario& scenario) {
  std::string out = e.machine ? scenario.machines.at(*e.machine).name : "any machine";
  out += " " + e.label;
  if (e.value) out += " " + std::to_string(*e.value);
  if (e.text) out += " \"" + *e.text + "\"";
  if (e.after != 0) out += " at or after " + std::to_string(e.after) + " ns";
  if (e.before != kNever) out += " at or before " + std::to_string(e.before) + " ns";
  return out;
}

}  // namespace

uint32_t Scenario::machine_id(std::string_view machine) const {
  for (uint32_t id = 0; id < machines.size(); ++id) {
    if (machines[id].name == machine) return id;
  }
  throw std::invalid_argument("scenario " + name + " has no machine named " + std::string(machine));
}

Scenario parse_scenario(std::string_view text, const fs::path& source, const std::vector<fs::path>& image_dirs) {
  return Parser(source, image_dirs).parse(text);
}

Scenario load_scenario(const fs::path& file, const std::vector<fs::path>& image_dirs) {
  std::ifstream in(file, std::ios::binary);
  if (!in || !fs::is_regular_file(file)) throw std::runtime_error("cannot read " + file.string());
  const std::string text(std::istreambuf_iterator<char>(in), {});
  if (in.bad()) throw std::runtime_error("cannot read " + file.string());
  return parse_scenario(text, file, image_dirs);
}

void schedule_input(World& world, const Input& input) {
  world.at(input.at, [&world, input] { apply(world.machine(input.target.machine), input); });
}

std::vector<std::string> check_expectations(const Scenario& scenario, const Trace& trace) {
  std::vector<std::string> failures;
  for (const Expectation& e : scenario.expectations) {
    const auto found = std::ranges::find_if(trace.records(), [&e](const TraceRecord& r) {
      return r.label == e.label && r.at >= e.after && r.at <= e.before && (!e.machine || r.machine == *e.machine) &&
             (!e.value || r.value == *e.value) && (!e.text || r.text == e.text);
    });
    if (e.present && found == trace.records().end()) {
      failures.push_back(e.name + ": expected " + describe(e, scenario) + ", but it never happened");
    } else if (!e.present && found != trace.records().end()) {
      failures.push_back(e.name + ": expected no " + describe(e, scenario) + ", but got: " + format(*found));
    }
  }
  return failures;
}

void build_world(const Scenario& scenario, World& world, const PlantFactories& plants) {
  if (scenario.stall_limit) world.set_stall_limit(*scenario.stall_limit);
  for (const MachineSpec& machine : scenario.machines) {
    if (machine.image) {
      world.add_machine(*machine.image, machine.devices);
    } else {
      world.add_passive_machine(machine.devices);
    }
  }
  for (const Wire& wire : scenario.wires) world.connect(wire);
  for (const Input& input : scenario.inputs) schedule_input(world, input);
  for (const BridgeSpec& bridge : scenario.bridges) {
    switch (bridge.kind) {
      case BridgeSpec::Kind::Udp: world.bridge(bridge.endpoint, udp_link(bridge.local, bridge.remote)); break;
      case BridgeSpec::Kind::TcpListen: world.bridge(bridge.endpoint, tcp_listen_link(bridge.address)); break;
      case BridgeSpec::Kind::TcpConnect: world.bridge(bridge.endpoint, tcp_connect_link(bridge.address)); break;
      case BridgeSpec::Kind::Tap: world.bridge(bridge.endpoint, tap_link(bridge.interface)); break;
    }
  }
  for (const PlantSpec& plant : scenario.plants) {
    const auto factory = plants.find(plant.type);
    if (factory == plants.end()) throw std::runtime_error("unknown plant type '" + plant.type + "'");
    world.add_plant(factory->second(plant.params, scenario), plant.period);
  }
}

}  // namespace costar
