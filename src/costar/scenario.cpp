#include "costar/scenario.hpp"

#include <algorithm>
#include <cctype>
#include <charconv>
#include <cmath>
#include <format>
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

// A sweep's range may have at most this many values.
constexpr uint64_t kMaxSweepValues = 1'000'000;

// Reads one scenario file, strictly: unknown keys, wrong types and values
// outside their range are errors that name the file and the table.
// Machines are parsed first so that wires and inputs can refer to them.
class Parser {
 public:
  Parser(const fs::path& source, const std::vector<fs::path>& image_dirs, bool check_sweeps = true)
      : source_(source), dir_(source.parent_path()), image_dirs_(image_dirs), check_sweeps_(check_sweeps) {}

  Scenario parse(std::string_view text, const std::vector<Override>& overrides) {
    toml::table root;
    try {
      root = toml::parse(text, source_.string());
    } catch (const toml::parse_error& error) {
      fail(std::string(error.description()));
    }
    for (const Override& o : overrides) set(root, o);
    return parse(root);
  }

 private:
  Scenario parse(const toml::table& root) {
    const std::string top = "the scenario";
    allow_keys(root, top, {"name", "duration_ms", "duration_us", "duration_ns", "machine", "wire", "model", "input",
                           "plant", "bridge", "expect", "stall_limit", "stack_check", "sweep", "max_runs"});
    scenario_.name = optional_string(root, "name", top).value_or(source_.stem().string());
    scenario_.file = source_;
    scenario_.duration = time(root, "duration", top);
    if (const auto limit = optional_integer(root, "stall_limit", top, 0, std::numeric_limits<int64_t>::max())) {
      scenario_.stall_limit = static_cast<uint64_t>(*limit);
    }
    if (const toml::node* check = root.get("stack_check")) scenario_.stack_check = stack_check(*check, {}, top);
    for_each_table(root, "machine", [&](const toml::table& t) { scenario_.machines.push_back(machine(t)); });
    for_each_table(root, "wire", [&](const toml::table& t) { scenario_.wires.push_back(wire(t)); });
    for_each_table(root, "model", [&](const toml::table& t) { scenario_.models.push_back(model(t)); });
    for_each_table(root, "input", [&](const toml::table& t) { scenario_.inputs.push_back(input(t)); });
    for_each_table(root, "plant", [&](const toml::table& t) { scenario_.plants.push_back(plant(t)); });
    for_each_table(root, "bridge", [&](const toml::table& t) { scenario_.bridges.push_back(bridge(t)); });
    if (const toml::node* expect = root.get("expect")) {
      if (!expect->is_table()) fail("[expect] must be a table");
      const toml::table& t = *expect->as_table();
      allow_keys(t, "[expect]", {"trace", "trace_machines", "trace_labels", "trace_exclude_labels", "event", "no"});
      if (const auto trace = optional_string(t, "trace", "[expect]")) scenario_.expected_trace = dir_ / *trace;
      golden_filter(t);
      for (const bool present : {true, false}) {
        size_t index = 0;
        for_each_table(t, present ? "event" : "no", [&](const toml::table& e) {
          scenario_.expectations.push_back(expectation(e, present, ++index));
        });
      }
    }
    // max_runs first: a range stops there.
    if (const auto max_runs = optional_integer(root, "max_runs", top, 1, std::numeric_limits<int64_t>::max())) {
      scenario_.max_runs = static_cast<uint64_t>(*max_runs);
    }
    for_each_table(root, "sweep", [&](const toml::table& t) { scenario_.sweeps.push_back(sweep(t)); });
    if (!scenario_.sweeps.empty()) check_sweeps(root);
    return std::move(scenario_);
  }
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

  bool optional_bool(const toml::table& t, std::string_view key, const std::string& what, bool fallback = false) const {
    const toml::node* node = t.get(key);
    if (node == nullptr) return fallback;
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

  // stack_check = true | false | { allowance = 2.0, extra_bytes = 8192 }:
  // `base`, switched on or off, or on with the given values.
  StackCheck stack_check(const toml::node& node, StackCheck base, const std::string& what) const {
    const std::string key = what + ": stack_check";
    if (const auto enabled = node.value_exact<bool>()) {
      base.enabled = *enabled;
      return base;
    }
    if (!node.is_table()) fail(key + " must be true, false or a table");
    const toml::table& t = *node.as_table();
    allow_keys(t, key, {"allowance", "extra_bytes"});
    base.enabled = true;
    if (const toml::node* allowance = t.get("allowance")) {
      const std::optional<double> value = allowance->is_number() ? allowance->value<double>() : std::nullopt;
      if (!value || !(*value > 0 && *value <= 1000)) fail(key + ": allowance must be a number above 0, at most 1000");
      base.allowance = *value;
    }
    if (const auto extra = optional_integer(t, "extra_bytes", key, 0, int64_t{1} << 30)) {
      base.extra = static_cast<size_t>(*extra);
    }
    return base;
  }

  MachineSpec machine(const toml::table& t) const {
    MachineSpec spec{.name = required_string(t, "name", "a machine")};
    const std::string what = "machine '" + spec.name + "'";
    allow_keys(t, what, {"name", "image", "devices", "stack_check", "cpu"});
    for (const MachineSpec& other : scenario_.machines) {
      if (other.name == spec.name) fail("two machines are named '" + spec.name + "'");
    }
    if (const auto image = optional_string(t, "image", what)) spec.image = find_image(*image, what);
    if (const toml::node* check = t.get("stack_check")) {
      if (!spec.image) fail(what + ": stack_check needs an image (a passive machine has no threads)");
      spec.stack_check = stack_check(*check, scenario_.stack_check.value_or(StackCheck{}), what);
    }
    if (const toml::node* cpu = t.get("cpu")) {
      // cpu = { ns_per_block = 12.5 }: what a basic block of instrumented
      // code costs (INSTRUMENT), to the picosecond.
      const std::string key = what + ": cpu";
      if (!spec.image) fail(key + " needs an image (a passive machine has no CPU)");
      if (!cpu->is_table()) fail(key + " must be a table");
      allow_keys(*cpu->as_table(), key, {"ns_per_block"});
      const toml::node* ns = cpu->as_table()->get("ns_per_block");
      const std::optional<double> value = ns != nullptr && ns->is_number() ? ns->value<double>() : std::nullopt;
      if (!value || !(*value >= 0.001 && *value <= 1e6)) {
        fail(key + ": ns_per_block must be a number from 0.001 to 1000000");
      }
      spec.block_ps = static_cast<uint64_t>(std::llround(*value * 1000));
    }
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
    if (kind == "registers" || kind == "watchdog") {
      spec.kind = DeviceKind::Registers;
      spec.watchdog = kind == "watchdog";
    } else if (kind == "memory") {
      spec.kind = DeviceKind::Memory;
      const auto size = optional_integer(t, "size", what, 1, int64_t{1} << 32);
      if (!size) fail(what + ": a memory device needs a size");
      spec.size = static_cast<size_t>(*size);
    } else if (kind != "channel") {
      fail(what + ": kind must be channel, registers, memory or watchdog");
    }
    if (spec.kind != DeviceKind::Memory && t.contains("size")) fail(what + ": only memory devices have a size");
    if (spec.watchdog && t.contains("irq")) fail(what + ": a watchdog has no irq (it resets the machine)");
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

  ModelSpec model(const toml::table& t) const {
    const std::string what = "model " + std::to_string(scenario_.models.size() + 1);
    const std::string type = required_string(t, "type", what);
    const std::string bus = required_string(t, "bus", what);
    ModelSpec spec{.bus = endpoint(bus, DeviceKind::Channel, what), .part = I2cRegistersSpec{}};
    if (scenario_.machines[spec.bus.machine].image) {
      fail(what + ": its bus " + bus + " must be a device of a passive machine (one without an image)");
    }
    const auto device = [&](std::string_view key, DeviceKind kind) {
      return endpoint(required_string(t, key, what), kind, what);
    };
    const auto i2c_address = [&] {
      const auto address = optional_integer(t, "address", what, 0, 0x7f);
      if (!address) fail(what + " needs its 7-bit I2C address");
      return static_cast<uint8_t>(*address);
    };
    if (type == "i2c-registers") {
      allow_keys(t, what, {"type", "bus", "address", "registers", "address_bytes", "register_bytes", "little_endian",
                           "auto_increment", "faults"});
      spec.part = I2cRegistersSpec{
          .address = i2c_address(),
          .registers = device("registers", DeviceKind::Registers),
          .address_bytes = static_cast<uint32_t>(optional_integer(t, "address_bytes", what, 1, 2).value_or(1)),
          .register_bytes = static_cast<uint32_t>(optional_integer(t, "register_bytes", what, 1, 4).value_or(1)),
          .little_endian = optional_bool(t, "little_endian", what),
          .auto_increment = optional_bool(t, "auto_increment", what, true),
          .nacks = nacks(t, what)};
    } else if (type == "i2c-eeprom") {
      allow_keys(t, what, {"type", "bus", "address", "memory", "address_bytes", "page_size", "write_time_ms",
                           "write_time_us", "write_time_ns", "faults"});
      I2cEepromSpec part{.address = i2c_address(), .memory = device("memory", DeviceKind::Memory)};
      const size_t size = device_of(part.memory).size;
      part.address_bytes = static_cast<uint32_t>(optional_integer(t, "address_bytes", what, 1, 2).value_or(1));
      const auto page_size = optional_integer(t, "page_size", what, 1, static_cast<int64_t>(size));
      if (!page_size) fail(what + " needs its page_size");
      part.page_size = static_cast<uint32_t>(*page_size);
      part.write_time = time(t, "write_time", what).value_or(0);
      part.nacks = nacks(t, what);
      if (part.address + i2c_eeprom_address_count(part, size) > 0x80) {
        fail(what + ": its memory needs more I2C addresses than there are above " + std::to_string(part.address));
      }
      spec.part = std::move(part);
    } else if (type == "spi-nor") {
      allow_keys(t, what, {"type", "bus", "chip_select", "memory", "jedec_id", "page_size", "program_time_ms",
                           "program_time_us", "program_time_ns", "erase_time_ms", "erase_time_us", "erase_time_ns"});
      const auto chip_select = optional_integer(t, "chip_select", what, 0, 255);
      const auto jedec_id = optional_integer(t, "jedec_id", what, 0, 0xffffff);
      if (!chip_select || !jedec_id) fail(what + " needs its chip_select and jedec_id");
      SpiNorSpec part{.chip_select = static_cast<uint8_t>(*chip_select),
                      .memory = device("memory", DeviceKind::Memory),
                      .jedec_id = static_cast<uint32_t>(*jedec_id)};
      const size_t size = device_of(part.memory).size;
      if (size > size_t{1} << 24) fail(what + ": a SPI NOR flash holds at most 16 MiB (3-byte addresses)");
      part.page_size = static_cast<uint32_t>(
          optional_integer(t, "page_size", what, 1, static_cast<int64_t>(size)).value_or(std::min<size_t>(256, size)));
      part.program_time = time(t, "program_time", what).value_or(0);
      part.erase_time = time(t, "erase_time", what).value_or(0);
      spec.part = std::move(part);
    } else {
      fail(what + ": type must be i2c-registers, i2c-eeprom or spi-nor");
    }
    // The other models on its bus are of its kind, and answer other
    // addresses or chip selects.
    const auto [first, count] = addresses(spec);
    for (const ModelSpec& other : scenario_.models) {
      if (other.bus != spec.bus) continue;
      if (is_spi(other) != is_spi(spec)) fail(what + ": I2C and SPI models can't share " + bus);
      const auto [other_first, other_count] = addresses(other);
      if (first < other_first + other_count && other_first < first + count) {
        fail(what + ": another model on " + bus + " answers " + (is_spi(spec) ? "chip select " : "I2C address ") +
             std::to_string(std::max(first, other_first)));
      }
    }
    return spec;
  }

  static bool is_spi(const ModelSpec& spec) { return std::holds_alternative<SpiNorSpec>(spec.part); }

  // The I2C addresses (or SPI chip selects) a model answers: the first, and
  // how many.
  std::pair<uint32_t, uint32_t> addresses(const ModelSpec& spec) const {
    if (const auto* part = std::get_if<I2cEepromSpec>(&spec.part)) {
      return {part->address, i2c_eeprom_address_count(*part, device_of(part->memory).size)};
    }
    if (const auto* part = std::get_if<SpiNorSpec>(&spec.part)) return {part->chip_select, 1};
    return {std::get<I2cRegistersSpec>(spec.part).address, 1};
  }

  // A model's `faults`: windows in which it NACKs its address.
  std::vector<NackWindow> nacks(const toml::table& t, const std::string& what) const {
    std::vector<NackWindow> out;
    const toml::node* faults = t.get("faults");
    if (faults == nullptr) return out;
    if (!faults->is_array()) fail(what + ": faults must be an array of tables");
    for (const toml::node& node : *faults->as_array()) {
      if (!node.is_table()) fail(what + ": each fault must be a table");
      const toml::table& f = *node.as_table();
      const std::string fault = what + ", fault";
      allow_keys(f, fault, {"kind", "from_ms", "from_us", "from_ns", "until_ms", "until_us", "until_ns"});
      if (required_string(f, "kind", fault) != "nack") fail(fault + ": kind must be nack");
      out.push_back({.from = time(f, "from", fault).value_or(0), .until = time(f, "until", fault).value_or(kNever)});
    }
    return out;
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
    allow_keys(t, what, {"endpoint", "kind", "local", "remote", "address", "framing", "interface", "link"});
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
      only({"endpoint", "kind", "address", "framing"});
      spec.kind = kind == "tcp-listen" ? BridgeSpec::Kind::TcpListen : BridgeSpec::Kind::TcpConnect;
      spec.address = required_string(t, "address", what);
      const std::string framing = optional_string(t, "framing", what).value_or("length");
      if (framing == "raw") {
        spec.framing = Framing::Raw;
      } else if (framing != "length") {
        fail(what + ": framing must be length or raw");
      }
    } else if (kind == "tap") {
      only({"endpoint", "kind", "interface"});
      spec.kind = BridgeSpec::Kind::Tap;
      spec.interface = required_string(t, "interface", what);
    } else if (kind == "pty") {
      only({"endpoint", "kind", "link"});
      spec.kind = BridgeSpec::Kind::Pty;
      spec.link = optional_string(t, "link", what).value_or("");
      if (!spec.link.empty()) spec.link = (dir_ / spec.link).string();
    } else {
      fail(what + ": kind must be udp, tcp-listen, tcp-connect, tap or pty");
    }
    return spec;
  }

  std::vector<std::string> strings(const toml::table& t, std::string_view key, const std::string& what) const {
    std::vector<std::string> out;
    const toml::node* node = t.get(key);
    if (node == nullptr) return out;
    if (!node->is_array()) fail(what + ": " + std::string(key) + " must be an array of strings");
    for (const toml::node& element : *node->as_array()) {
      if (!element.is_string()) fail(what + ": " + std::string(key) + " must be an array of strings");
      out.push_back(*element.value<std::string>());
    }
    return out;
  }

  // What the golden trace covers: trace_machines, trace_labels and
  // trace_exclude_labels, as `costar run`'s --machine, --label and
  // --exclude-label.
  void golden_filter(const toml::table& t) {
    const std::string what = "[expect]";
    TraceFilter& filter = scenario_.golden_filter;
    for (const std::string& machine : strings(t, "trace_machines", what)) {
      filter.machines.push_back(machine_id(machine, what + ": trace_machines"));
    }
    filter.labels = strings(t, "trace_labels", what);
    filter.excluded_labels = strings(t, "trace_exclude_labels", what);
    if (!filter.empty() && !scenario_.expected_trace) fail(what + ": trace_machines and trace_labels go with trace");
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

  // ── Overrides and sweeps ──────────────────────────────────────────

  // Set `o.value` at `o.key` in the file's tables (see Override).
  void set(toml::table& root, const Override& o) const {
    const std::string what = "--set " + o.key;
    std::vector<std::string> steps;
    for (size_t start = 0;;) {
      const size_t dot = o.key.find('.', start);
      steps.push_back(o.key.substr(start, dot - start));
      if (steps.back().empty()) fail(what + ": a key is names separated by '.'");
      if (dot == std::string::npos) break;
      start = dot + 1;
    }
    toml::node* node = &root;
    std::string path;  // the steps taken, for messages
    for (size_t i = 0; i < steps.size();) {
      const std::string where = path.empty() ? "the scenario" : path;
      std::string taken = steps[i];
      if (toml::table* table = node->as_table()) {
        if (i + 1 == steps.size()) {
          assign(*table, steps[i], o.value);
          return;
        }
        node = table->get(steps[i]);
        if (node == nullptr) fail(what + ": " + where + " has no " + steps[i]);
        i += 1;
      } else if (toml::array* array = node->as_array()) {
        const auto [index, used] = element(*array, steps, i, what, where);
        if (i + used == steps.size()) {
          if (used != 1) fail(what + ": a key must follow " + steps[i] + "." + steps[i + 1]);
          with_value(o.value, [&](const auto& value) {
            array->replace(array->cbegin() + static_cast<ptrdiff_t>(index), value);
          });
          return;
        }
        node = array->get(index);
        taken = std::to_string(index + 1);
        i += used;
      } else {
        fail(what + ": " + where + " is a value, not a table");
      }
      path += (path.empty() ? "" : ".") + taken;
    }
  }

  // The element of `array` that steps[i] (and maybe steps[i + 1]) select: a
  // position from 1, a table's `name`, or a wire's endpoint.  Returns its
  // index and how many steps it took.
  std::pair<size_t, size_t> element(const toml::array& array, const std::vector<std::string>& steps, size_t i,
                                    const std::string& what, const std::string& where) const {
    const std::string& step = steps[i];
    size_t position = 0;
    const auto [end, error] = std::from_chars(step.data(), step.data() + step.size(), position);
    if (error == std::errc() && end == step.data() + step.size()) {
      if (position < 1 || position > array.size()) {
        fail(what + ": " + where + " has " + std::to_string(array.size()) + " element(s), not " + step);
      }
      return {position - 1, 1};
    }
    for (size_t index = 0; index < array.size(); ++index) {
      const toml::table* table = array.get(index)->as_table();
      if (table != nullptr && table->get("name") != nullptr && table->get("name")->value<std::string>() == step) {
        return {index, 1};
      }
    }
    // A wire by one of its endpoints: two steps, machine and device.
    if (i + 1 < steps.size()) {
      const std::string endpoint = step + "." + steps[i + 1];
      std::optional<size_t> found;
      for (size_t index = 0; index < array.size(); ++index) {
        const toml::table* table = array.get(index)->as_table();
        const toml::array* endpoints = table != nullptr && table->get("endpoints") != nullptr
                                           ? table->get("endpoints")->as_array()
                                           : nullptr;
        if (endpoints == nullptr) continue;
        for (const toml::node& e : *endpoints) {
          if (e.value<std::string>() != endpoint) continue;
          if (found) fail(what + ": more than one of " + where + " has the endpoint " + endpoint);
          found = index;
        }
      }
      if (found) return {*found, 2};
    }
    fail(what + ": " + where + " has nothing named " + step);
  }

  // Call `f` with `text` as a TOML value, or as a string if it isn't one.
  template <class F>
  static void with_value(const std::string& text, F&& f) {
    toml::table parsed;
    try {
      parsed = toml::parse("value = " + text);
    } catch (const toml::parse_error&) {
    }
    if (parsed.size() != 1 || !parsed.contains("value")) parsed = toml::table{{"value", text}};
    parsed.get("value")->visit(f);
  }

  // `table.key = text`.  A time replaces the same time in other units.
  static void assign(toml::table& table, const std::string& key, const std::string& text) {
    for (const std::string_view suffix : {"_ms", "_us", "_ns"}) {
      if (!key.ends_with(suffix)) continue;
      const std::string base = key.substr(0, key.size() - suffix.size());
      for (const char* other : {"_ms", "_us", "_ns"}) table.erase(base + other);
    }
    with_value(text, [&](const auto& value) { table.insert_or_assign(key, value); });
  }

  Sweep sweep(const toml::table& t) const {
    const std::string what = "sweep " + std::to_string(scenario_.sweeps.size() + 1);
    allow_keys(t, what, {"key", "values", "range"});
    Sweep out{.key = required_string(t, "key", what), .values = {}};
    const toml::node* values = t.get("values");
    const toml::node* range = t.get("range");
    if ((values == nullptr) == (range == nullptr)) fail(what + ": needs one of values and range");
    if (values != nullptr) {
      if (!values->is_array() || values->as_array()->empty()) fail(what + ": values must be a non-empty array");
      for (const toml::node& value : *values->as_array()) {
        if (value.is_array() || value.is_table()) fail(what + ": each value must be a number, string or boolean");
        if (value.is_floating_point()) {
          out.values.push_back(float_text(*value.value<double>()));
          continue;
        }
        std::ostringstream text;
        value.visit([&text](const auto& v) { text << v; });
        out.values.push_back(text.str());
      }
      return out;
    }
    const toml::array* bounds = range->as_array();
    if (bounds == nullptr || bounds->size() != 3 || !std::ranges::all_of(*bounds, [](const toml::node& n) {
          return n.is_number();
        })) {
      fail(what + ": range must be [from, to, step], numbers");
    }
    // A range has at most max_runs values (it would have more combinations),
    // and kMaxSweepValues in any case: it is generated in full.
    const uint64_t limit = std::min(scenario_.max_runs, kMaxSweepValues);
    const auto too_many = [&] {
      fail(what + (limit == kMaxSweepValues ? ": range has too many values"
                                            : ": range has more values than max_runs (" +
                                                  std::to_string(scenario_.max_runs) + ")"));
    };
    if (std::ranges::all_of(*bounds, [](const toml::node& n) { return n.is_integer(); })) {
      const int64_t from = *(*bounds)[0].value<int64_t>(), to = *(*bounds)[1].value<int64_t>();
      const int64_t step = *(*bounds)[2].value<int64_t>();
      if (step <= 0 || from > to) fail(what + ": range needs from <= to and a positive step");
      // Distances in unsigned arithmetic: they may not fit in an int64_t.
      const auto distance = [](int64_t a, int64_t b) { return static_cast<uint64_t>(b) - static_cast<uint64_t>(a); };
      if (distance(from, to) / static_cast<uint64_t>(step) >= limit) too_many();
      for (int64_t v = from;; v += step) {
        out.values.push_back(std::to_string(v));
        if (distance(v, to) < static_cast<uint64_t>(step)) break;
      }
      return out;
    }
    const double from = *(*bounds)[0].value<double>(), to = *(*bounds)[1].value<double>();
    const double step = *(*bounds)[2].value<double>();
    if (!std::isfinite(from) || !std::isfinite(to) || !std::isfinite(step) || !(step > 0) || !(from <= to)) {
      fail(what + ": range needs finite numbers, from <= to and a positive step");
    }
    // The end is included if a step lands on it, give or take rounding, and
    // values are rounded to the decimals the range was written with.
    int decimals = 0;
    for (const double bound : {from, to, step}) {
      const std::string text = std::format("{}", bound);
      if (text.find('e') != std::string::npos) decimals = -1;  // shortest form it is
      const size_t dot = text.find('.');
      if (decimals >= 0 && dot != std::string::npos) {
        decimals = std::max(decimals, static_cast<int>(text.size() - dot - 1));
      }
    }
    // Each value must be above the last: a step too small for the values'
    // precision would repeat one forever.
    double last = 0;
    for (uint64_t i = 0;; ++i) {
      double value = from + static_cast<double>(i) * step;
      if (!std::isfinite(value) || value - to > step * 1e-9) break;
      if (decimals >= 0) value = std::stod(std::format("{:.{}f}", value, decimals));
      if (i > 0 && !(value > last)) fail(what + ": range does not advance (its step is too small for its values)");
      if (i == limit) too_many();
      out.values.push_back(float_text(value));
      last = value;
    }
    return out;
  }

  // The shortest text that reads back as `value`, and as a TOML float.
  static std::string float_text(double value) {
    std::string text = std::format("{}", value);
    if (text.find_first_of(".eEn") == std::string::npos) text += ".0";  // "nan" and "inf" have an n
    return text;
  }

  // Sweeps are checked when the scenario is loaded, like the rest of it:
  // each value parses on its own, and there aren't too many combinations.
  void check_sweeps(const toml::table& root) const {
    uint64_t runs = 1;
    for (const Sweep& sweep : scenario_.sweeps) {
      if (runs > scenario_.max_runs / sweep.values.size()) {
        fail("the sweeps have more combinations than max_runs (" + std::to_string(scenario_.max_runs) + ")");
      }
      runs *= sweep.values.size();
    }
    if (!check_sweeps_) return;
    for (size_t i = 0; i < scenario_.sweeps.size(); ++i) {
      for (const std::string& value : scenario_.sweeps[i].values) {
        toml::table copy = root;
        const Override o{scenario_.sweeps[i].key, value};
        try {
          Parser parser(source_, image_dirs_, false);
          parser.set(copy, o);
          parser.parse(copy);
        } catch (const std::runtime_error& error) {
          fail("sweep " + std::to_string(i + 1) + " (" + o.text() + "): " + without_source(error.what()));
        }
      }
    }
  }

  // An error's message without the "<source>: " this parser puts first.
  std::string without_source(std::string_view message) const {
    const std::string prefix = source_.string() + ": ";
    return std::string(message.starts_with(prefix) ? message.substr(prefix.size()) : message);
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
  bool check_sweeps_;
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

Override parse_override(std::string_view text) {
  const size_t equals = text.find('=');
  if (equals == std::string_view::npos || equals == 0) {
    throw std::invalid_argument("--set needs KEY=VALUE, not '" + std::string(text) + "'");
  }
  return {std::string(text.substr(0, equals)), std::string(text.substr(equals + 1))};
}

Scenario parse_scenario(std::string_view text, const fs::path& source, const std::vector<fs::path>& image_dirs,
                        const std::vector<Override>& overrides) {
  return Parser(source, image_dirs).parse(text, overrides);
}

std::vector<std::vector<Override>> sweep_combinations(const Scenario& scenario) {
  if (scenario.sweeps.empty()) return {};
  std::vector<std::vector<Override>> out{{}};
  for (const Sweep& sweep : scenario.sweeps) {
    std::vector<std::vector<Override>> next;
    for (const std::vector<Override>& combination : out) {
      for (const std::string& value : sweep.values) {
        next.push_back(combination);
        next.back().push_back({sweep.key, value});
      }
    }
    out = std::move(next);
  }
  return out;
}

Scenario load_scenario(const fs::path& file, const std::vector<fs::path>& image_dirs,
                       const std::vector<Override>& overrides) {
  std::ifstream in(file, std::ios::binary);
  if (!in || !fs::is_regular_file(file)) throw std::runtime_error("cannot read " + file.string());
  const std::string text(std::istreambuf_iterator<char>(in), {});
  if (in.bad()) throw std::runtime_error("cannot read " + file.string());
  return parse_scenario(text, file, image_dirs, overrides);
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
  if (scenario.stack_check) world.set_stack_check(*scenario.stack_check);
  for (const MachineSpec& machine : scenario.machines) {
    Machine& added =
        machine.image ? world.add_machine(*machine.image, machine.devices) : world.add_passive_machine(machine.devices);
    added.set_name(machine.name);
    if (machine.stack_check) added.set_stack_check(*machine.stack_check);
    if (machine.block_ps) added.set_block_ps(*machine.block_ps);
  }
  for (const Wire& wire : scenario.wires) world.connect(wire);
  add_models(world, scenario.models);
  for (const Input& input : scenario.inputs) schedule_input(world, input);
  for (const BridgeSpec& bridge : scenario.bridges) {
    switch (bridge.kind) {
      case BridgeSpec::Kind::Udp: world.bridge(bridge.endpoint, udp_link(bridge.local, bridge.remote)); break;
      case BridgeSpec::Kind::TcpListen:
        world.bridge(bridge.endpoint, tcp_listen_link(bridge.address, bridge.framing));
        break;
      case BridgeSpec::Kind::TcpConnect:
        world.bridge(bridge.endpoint, tcp_connect_link(bridge.address, bridge.framing));
        break;
      case BridgeSpec::Kind::Tap: world.bridge(bridge.endpoint, tap_link(bridge.interface)); break;
      case BridgeSpec::Kind::Pty: world.bridge(bridge.endpoint, pty_link(bridge.link)); break;
    }
  }
  for (const PlantSpec& plant : scenario.plants) {
    const auto factory = plants.find(plant.type);
    if (factory == plants.end()) throw std::runtime_error("unknown plant type '" + plant.type + "'");
    world.add_plant(factory->second(plant.params, scenario), plant.period);
  }
}

}  // namespace costar
