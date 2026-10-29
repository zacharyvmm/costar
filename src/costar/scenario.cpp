#include "costar/scenario.hpp"

#include <cctype>
#include <cmath>
#include <fstream>
#include <sstream>
#include <stdexcept>

#include <toml++/toml.hpp>

#include "costar/world.hpp"

namespace costar {

namespace fs = std::filesystem;

namespace {

// Reads one scenario file.  Machines are parsed first so that wires and
// inputs can refer to them by name.
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

    scenario_.name = root["name"].value_or(source_.stem().string());
    scenario_.duration = time(root, "duration");
    for_each_table(root, "machine", [&](const toml::table& t) { scenario_.machines.push_back(machine(t)); });
    for_each_table(root, "wire", [&](const toml::table& t) { scenario_.wires.push_back(wire(t)); });
    for_each_table(root, "input", [&](const toml::table& t) { scenario_.inputs.push_back(input(t)); });
    for_each_table(root, "plant", [&](const toml::table& t) { scenario_.plants.push_back(plant(t)); });
    if (const auto trace = root["expect"]["trace"].value<std::string>()) scenario_.expected_trace = dir_ / *trace;
    return std::move(scenario_);
  }

 private:
  [[noreturn]] void fail(const std::string& what) const { throw std::runtime_error(source_.string() + ": " + what); }

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

  std::string required_string(const toml::table& t, std::string_view key, std::string_view what) const {
    const auto value = t[key].value<std::string>();
    if (!value) fail(std::string(what) + " needs a string '" + std::string(key) + "'");
    return *value;
  }

  // `<key>_ms`, `<key>_us` or `<key>_ns`.
  std::optional<Time> time(const toml::table& t, std::string_view key) const {
    const std::string name(key);
    if (const auto ms = t[name + "_ms"].value<double>()) return static_cast<Time>(std::llround(*ms * 1e6));
    if (const auto us = t[name + "_us"].value<double>()) return static_cast<Time>(std::llround(*us * 1e3));
    if (const auto ns = t[name + "_ns"].value<int64_t>()) return static_cast<Time>(*ns);
    return std::nullopt;
  }

  // `<key> = "text"` or `<key>_hex = "0a 0b"`.
  std::optional<Frame> bytes(const toml::table& t, std::string_view key) const {
    const std::string name(key);
    if (const auto text = t[name].value<std::string>()) return Frame(text->begin(), text->end());
    const auto hex = t[name + "_hex"].value<std::string>();
    if (!hex) return std::nullopt;
    Frame out;
    std::string digits;
    for (char c : *hex) {
      if (c == ' ' || c == ':' || c == '-') continue;
      if (!std::isxdigit(static_cast<unsigned char>(c))) fail("bad hex digit in " + name + "_hex");
      digits += c;
    }
    if (digits.size() % 2 != 0) fail(name + "_hex needs an even number of digits");
    for (size_t i = 0; i < digits.size(); i += 2) {
      out.push_back(static_cast<uint8_t>(std::stoul(digits.substr(i, 2), nullptr, 16)));
    }
    return out;
  }

  // "machine.device"
  Endpoint endpoint(const std::string& text) const {
    const size_t dot = text.find('.');
    if (dot == std::string::npos) fail("'" + text + "' should be machine.device");
    const std::string machine = text.substr(0, dot);
    for (uint32_t id = 0; id < scenario_.machines.size(); ++id) {
      if (scenario_.machines[id].name == machine) return {id, text.substr(dot + 1)};
    }
    fail("no machine named '" + machine + "'");
  }

  fs::path find_image(const std::string& image) const {
    std::vector<fs::path> candidates{dir_ / image};
    for (const fs::path& dir : image_dirs_) candidates.push_back(dir / image);
    for (const fs::path& candidate : candidates) {
      if (fs::exists(candidate)) return candidate;
    }
    fail("image '" + image + "' not found next to the scenario or in the image directories");
  }

  MachineSpec machine(const toml::table& t) const {
    MachineSpec spec{.name = required_string(t, "name", "a machine")};
    if (const auto image = t["image"].value<std::string>()) spec.image = find_image(*image);
    if (const toml::array* devices = t["devices"].as_array()) {
      for (const toml::node& node : *devices) {
        if (!node.is_table()) fail("devices of machine " + spec.name + " must be tables");
        spec.devices.push_back(device(*node.as_table()));
      }
    }
    return spec;
  }

  DeviceSpec device(const toml::table& t) const {
    DeviceSpec spec{.name = required_string(t, "name", "a device"), .kind = DeviceKind::Channel};
    const std::string kind = required_string(t, "kind", "device " + spec.name);
    if (kind == "registers") {
      spec.kind = DeviceKind::Registers;
    } else if (kind == "memory") {
      spec.kind = DeviceKind::Memory;
      spec.size = t["size"].value_or<int64_t>(0);
      if (spec.size == 0) fail("memory device " + spec.name + " needs a size");
    } else if (kind != "channel") {
      fail("device " + spec.name + ": kind must be channel, registers or memory");
    }
    if (const auto irq = t["irq"].value<int64_t>()) spec.irq = static_cast<unsigned>(*irq);
    return spec;
  }

  Wire wire(const toml::table& t) const {
    Wire w;
    const toml::array* endpoints = t["endpoints"].as_array();
    if (endpoints == nullptr || endpoints->size() < 2) fail("a wire needs at least two endpoints");
    for (const toml::node& node : *endpoints) {
      const auto text = node.value<std::string>();
      if (!text) fail("wire endpoints are \"machine.device\" strings");
      w.endpoints.push_back(endpoint(*text));
    }
    w.latency = time(t, "latency").value_or(0);
    w.bits_per_second = t["bits_per_second"].value_or<int64_t>(0);
    w.bits_per_byte = static_cast<uint32_t>(t["bits_per_byte"].value_or<int64_t>(8));
    w.byte_stream = t["byte_stream"].value_or(false);
    w.shared_medium = t["shared_medium"].value_or(false);
    if (const toml::array* faults = t["faults"].as_array()) {
      for (const toml::node& node : *faults) {
        const toml::table& f = *node.as_table();
        const std::string kind = required_string(f, "kind", "a fault");
        if (kind != "drop" && kind != "corrupt") fail("fault kind must be drop or corrupt");
        w.faults.push_back({kind == "drop" ? WireFault::Kind::Drop : WireFault::Kind::Corrupt,
                            time(f, "from").value_or(0), time(f, "until").value_or(kNever)});
      }
    }
    return w;
  }

  Input input(const toml::table& t) const {
    Input in{.kind = Input::Kind::Deliver,
             .at = time(t, "at").value_or(0),
             .target = endpoint(required_string(t, "target", "an input"))};
    int kinds = 0;
    if (auto data = bytes(t, "deliver")) {
      in.kind = Input::Kind::Deliver, in.data = *data, ++kinds;
    }
    if (auto data = bytes(t, "send")) {
      in.kind = Input::Kind::Send, in.data = *data, ++kinds;
    }
    if (auto data = bytes(t, "write")) {
      in.kind = Input::Kind::WriteMemory, in.data = *data, ++kinds;
      in.offset = static_cast<uint32_t>(t["offset"].value_or<int64_t>(0));
    }
    if (const auto reg = t["register"].value<int64_t>()) {
      in.kind = Input::Kind::SetRegister, ++kinds;
      in.reg = static_cast<uint32_t>(*reg);
      in.value = static_cast<uint32_t>(t["value"].value_or<int64_t>(0));
    }
    if (kinds != 1) fail("an input needs exactly one of deliver, send, write or register");
    return in;
  }

  PlantSpec plant(const toml::table& t) const {
    PlantSpec spec{.type = required_string(t, "type", "a plant"), .period = time(t, "period").value_or(0)};
    if (spec.period == 0) fail("plant " + spec.type + " needs a period");
    for (const auto& [key, node] : t) {
      const std::string name(key.str());
      if (name == "type" || name.starts_with("period_")) continue;
      if (const auto i = node.value_exact<int64_t>()) {
        spec.params[name] = *i;
      } else if (const auto d = node.value_exact<double>()) {
        spec.params[name] = *d;
      } else if (const auto s = node.value_exact<std::string>()) {
        spec.params[name] = *s;
      } else {
        fail("plant parameter " + name + " must be a number or a string");
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
  }
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
  std::ifstream in(file);
  if (!in) throw std::runtime_error("cannot read " + file.string());
  std::stringstream text;
  text << in.rdbuf();
  return parse_scenario(text.str(), file, image_dirs);
}

void build_world(const Scenario& scenario, World& world, const PlantFactories& plants) {
  for (const MachineSpec& machine : scenario.machines) {
    if (machine.image) {
      world.add_machine(*machine.image, machine.devices);
    } else {
      world.add_passive_machine(machine.devices);
    }
  }
  for (const Wire& wire : scenario.wires) world.connect(wire);
  for (const Input& input : scenario.inputs) {
    world.at(input.at, [&world, input] { apply(world.machine(input.target.machine), input); });
  }
  for (const PlantSpec& plant : scenario.plants) {
    const auto factory = plants.find(plant.type);
    if (factory == plants.end()) throw std::runtime_error("unknown plant type '" + plant.type + "'");
    world.add_plant(factory->second(plant.params, scenario), plant.period);
  }
}

}  // namespace costar
