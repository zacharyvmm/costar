#include "thermal_plant.hpp"

#include <algorithm>
#include <cstdint>
#include <stdexcept>
#include <string>

#include "costar/world.hpp"

namespace costar {

namespace {

class ThermalPlant : public Plant {
 public:
  ThermalPlant(uint32_t machine, std::string device, uint32_t sensor, uint32_t heater, int64_t temperature,
               int64_t heat, int64_t cool)
      : machine_(machine),
        device_(std::move(device)),
        sensor_(sensor),
        heater_(heater),
        temperature_(temperature),
        heat_(heat),
        cool_(cool) {}

  // Publish the temperature, then integrate one period with the heater's
  // current state.  The temperature stays within a register's range.
  void step(Time, World& world) override {
    Machine& machine = world.machine(machine_);
    const bool heating = machine.devices().get<Registers>(device_).values[heater_] != 0;
    machine.set_register(device_, sensor_, static_cast<uint32_t>(temperature_));
    temperature_ = std::clamp<int64_t>(heating ? temperature_ + heat_ : temperature_ - cool_, 0, UINT32_MAX);
  }

 private:
  uint32_t machine_;
  std::string device_;
  uint32_t sensor_;
  uint32_t heater_;
  int64_t temperature_;
  int64_t heat_;
  int64_t cool_;
};

// An integer parameter in 0..UINT32_MAX (registers, temperatures and rates
// all fit in a register).
int64_t integer(const PlantParams& params, const std::string& key, int64_t fallback) {
  const auto it = params.find(key);
  if (it == params.end()) return fallback;
  const auto* value = std::get_if<int64_t>(&it->second);
  if (value == nullptr || *value < 0 || *value > UINT32_MAX) {
    throw std::runtime_error("thermal plant: " + key + " must be an integer from 0 to 4294967295");
  }
  return *value;
}

}  // namespace

std::unique_ptr<Plant> make_thermal_plant(const PlantParams& params, const Scenario& scenario) {
  for (const auto& [key, value] : params) {
    if (key != "target" && key != "sensor" && key != "heater" && key != "initial" && key != "heat" && key != "cool") {
      throw std::runtime_error("thermal plant: unknown parameter '" + key + "'");
    }
  }
  const auto target = params.find("target");
  const auto* text = target == params.end() ? nullptr : std::get_if<std::string>(&target->second);
  if (text == nullptr || text->find('.') == std::string::npos) {
    throw std::runtime_error("thermal plant: target must be \"machine.device\"");
  }
  const size_t dot = text->find('.');
  return std::make_unique<ThermalPlant>(scenario.machine_id(text->substr(0, dot)), text->substr(dot + 1),
                                        static_cast<uint32_t>(integer(params, "sensor", 0)),
                                        static_cast<uint32_t>(integer(params, "heater", 1)),
                                        integer(params, "initial", 180), integer(params, "heat", 5),
                                        integer(params, "cool", 3));
}

}  // namespace costar
