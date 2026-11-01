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

#include "costar/cpu.hpp"
#include "costar/devices.hpp"
#include "costar/models.hpp"
#include "costar/host.hpp"
#include "costar/plant.hpp"
#include "costar/time.hpp"
#include "costar/trace.hpp"
#include "costar/wires.hpp"

namespace costar {

class World;

struct MachineSpec {
  std::string name;
  std::optional<std::filesystem::path> image = {};  // none: a passive machine
  std::vector<DeviceSpec> devices = {};
  std::optional<StackCheck> stack_check = {};       // none: the scenario's
  std::optional<uint64_t> block_ps = {};            // cpu.ns_per_block, in picoseconds; none: kDefaultBlockPs
};

// A host action at a virtual time, on one device or (Power) one machine.
struct Input {
  enum class Kind { Deliver, Send, SetRegister, WriteMemory, Power };
  enum class Power { Off, On, Reset };  // Reset: off, then on at once
  Kind kind;
  Time at;
  Endpoint target;      // Power: the device is empty
  Frame data = {};      // Deliver, Send, WriteMemory
  uint32_t reg = 0;     // SetRegister
  uint32_t value = 0;   // SetRegister
  uint32_t offset = 0;  // WriteMemory
  Power power = Power::Reset;
};

// A trace record that must ([[expect.event]]) or must not ([[expect.no]])
// appear between `after` and `before` (both inclusive).
struct Expectation {
  bool present;
  std::string label;
  std::optional<uint32_t> machine = {};  // none: any machine
  std::optional<uint32_t> value = {};    // the record's value (a frame's size for tx, rx, drop)
  std::optional<std::string> text = {};  // the record's text, exactly
  Time after = 0;
  Time before = kNever;
  std::string name = {};  // "expect.event 2", for messages
};

// A channel connected to the real host (see host.hpp).
struct BridgeSpec {
  enum class Kind { Udp, TcpListen, TcpConnect, Tap, Pty };
  Endpoint endpoint;
  Kind kind;
  std::string local = {};             // Udp: "host:port" to bind
  std::string remote = {};            // Udp: "host:port" to send to (optional)
  std::string address = {};           // TcpListen, TcpConnect
  Framing framing = Framing::Length;  // TcpListen, TcpConnect
  std::string interface = {};         // Tap
  std::string link = {};              // Pty: a symbolic link to the terminal (optional)
};

// A value set in a scenario file before it is parsed (`--set KEY=VALUE`, and
// sweeps).  KEY is a path into the file, '.'-separated:
//   duration_ms, stall_limit, ...      a top-level key
//   machine.<name>.<key>               a machine, by name; its devices are
//                                      machine.<name>.devices.<name>.<key>
//   wire.<n>.<key>                     the n-th wire (from 1), or
//   wire.<machine>.<device>.<key>      the one wire with that endpoint
//   wire.<n>.faults.<m>.<key>          a wire's fault
//   model.<n>.<key>, input.<n>.<key>, plant.<n>.<key>, bridge.<n>.<key>,
//   sweep.<n>.<key>
//   expect.<key>, expect.event.<n>.<key>, expect.no.<n>.<key>
// and, generally, a table's key, or an array's element by position (from 1)
// or by its `name`.  Every step but the last must exist (a missing table is
// set whole: stack_check={ allowance = 3.0 }).  Setting a time
// (`<key>_ms`) replaces the same time in other units (`<key>_us`, `<key>_ns`).
// VALUE is TOML (30, 1.5, "text", [1, 2], true); anything else is a string.
// The result is parsed as strictly as any scenario.
struct Override {
  std::string key;
  std::string value;

  // "key=value"
  std::string text() const { return key + "=" + value; }
  bool operator==(const Override&) const = default;
};

// "KEY=VALUE", as given to --set.
Override parse_override(std::string_view text);

// [[sweep]]: values for one key.  `costar test` runs the scenario with every
// combination of every sweep's values.
struct Sweep {
  std::string key;
  std::vector<std::string> values;  // each as TOML, for an Override
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
  std::filesystem::path file;  // where it was read from (its source)
  std::optional<Time> duration;
  std::optional<uint64_t> stall_limit;  // none: kDefaultStallLimit
  std::optional<StackCheck> stack_check;  // none: StackCheck's defaults
  std::vector<MachineSpec> machines;
  std::vector<Wire> wires;
  std::vector<ModelSpec> models;
  std::vector<Input> inputs;
  std::vector<PlantSpec> plants;
  std::vector<BridgeSpec> bridges;  // any bridge makes the scenario realtime
  std::optional<std::filesystem::path> expected_trace;
  TraceFilter golden_filter = {};  // what the golden trace covers (trace_machines, trace_labels, ...)
  std::vector<Expectation> expectations;
  std::vector<Sweep> sweeps;
  uint64_t max_runs = 1000;  // the most combinations the sweeps may have

  // The id a machine gets in the World (its position in the file).
  uint32_t machine_id(std::string_view name) const;
};

// Plants are linked into the program that runs scenarios; a scenario names
// one by its type.
using PlantFactory = std::function<std::unique_ptr<Plant>(const PlantParams&, const Scenario&)>;
using PlantFactories = std::map<std::string, PlantFactory, std::less<>>;

// Parse a scenario, with `overrides` set in it first, in order.  Relative
// paths are resolved against the directory of `source`; images are also
// looked for in `image_dirs`.  Every sweep value is checked too.  Throws
// std::runtime_error with the source name on errors.
Scenario parse_scenario(std::string_view text, const std::filesystem::path& source,
                        const std::vector<std::filesystem::path>& image_dirs = {},
                        const std::vector<Override>& overrides = {});
Scenario load_scenario(const std::filesystem::path& file, const std::vector<std::filesystem::path>& image_dirs = {},
                       const std::vector<Override>& overrides = {});

// Every combination of the scenario's sweep values, the last sweep varying
// fastest: the overrides for each run.  None if it has no sweeps.
std::vector<std::vector<Override>> sweep_combinations(const Scenario& scenario);

// Add the scenario's machines, wires, models, inputs and plants to `world`.
void build_world(const Scenario& scenario, World& world, const PlantFactories& plants = {});

// Apply `input` to its target machine at `input.at`.
void schedule_input(World& world, const Input& input);

// One message per expectation the trace fails.
std::vector<std::string> check_expectations(const Scenario& scenario, const Trace& trace);

}  // namespace costar
