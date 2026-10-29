#pragma once

#include <cstdint>
#include <filesystem>
#include <functional>
#include <map>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "costar/devices.hpp"
#include "costar/plant.hpp"
#include "costar/time.hpp"
#include "costar/wires.hpp"

namespace costar {

class World;

struct MachineSpec {
  std::string name;
  std::optional<std::filesystem::path> image = {};  // none: a passive machine
  std::vector<DeviceSpec> devices = {};
};

// A host action at a virtual time, on one device.
struct Input {
  enum class Kind { Deliver, Send, SetRegister, WriteMemory };
  Kind kind;
  Time at;
  Endpoint target;
  Frame data = {};      // Deliver, Send, WriteMemory
  uint32_t reg = 0;     // SetRegister
  uint32_t value = 0;   // SetRegister
  uint32_t offset = 0;  // WriteMemory
};

// A channel connected to the real host (see host.hpp).
struct BridgeSpec {
  enum class Kind { Udp, TcpListen, TcpConnect, Tap };
  Endpoint endpoint;
  Kind kind;
  std::string local = {};      // Udp: "host:port" to bind
  std::string remote = {};     // Udp: "host:port" to send to (optional)
  std::string address = {};    // TcpListen, TcpConnect
  std::string interface = {};  // Tap
};

using PlantParam = std::variant<int64_t, double, std::string>;
using PlantParams = std::map<std::string, PlantParam>;

struct PlantSpec {
  std::string type;
  Time period;
  PlantParams params = {};
};

// Everything a scenario file describes, with names resolved.
struct Scenario {
  std::string name;
  std::optional<Time> duration;
  std::vector<MachineSpec> machines;
  std::vector<Wire> wires;
  std::vector<Input> inputs;
  std::vector<PlantSpec> plants;
  std::vector<BridgeSpec> bridges;  // any bridge makes the scenario realtime
  std::optional<std::filesystem::path> expected_trace;

  // The id a machine gets in the World (its position in the file).
  uint32_t machine_id(std::string_view name) const;
};

// Plants are linked into the program that runs scenarios; a scenario names
// one by its type.
using PlantFactory = std::function<std::unique_ptr<Plant>(const PlantParams&, const Scenario&)>;
using PlantFactories = std::map<std::string, PlantFactory, std::less<>>;

// Parse a scenario.  Relative paths are resolved against the directory of
// `source`; images are also looked for in `image_dirs`.  Throws
// std::runtime_error with the source name on errors.
Scenario parse_scenario(std::string_view text, const std::filesystem::path& source,
                        const std::vector<std::filesystem::path>& image_dirs = {});
Scenario load_scenario(const std::filesystem::path& file, const std::vector<std::filesystem::path>& image_dirs = {});

// Add the scenario's machines, wires, inputs and plants to `world`.
void build_world(const Scenario& scenario, World& world, const PlantFactories& plants = {});

// Apply `input` to its target machine at `input.at`.
void schedule_input(World& world, const Input& input);

}  // namespace costar
