#pragma once

#include <memory>

#include "costar/scenario.hpp"

namespace costar {

// A room with a heater, as an example plant.  Scenario parameters:
//   target    "machine.device" of a register device (required)
//   sensor    register the temperature is written to (default 0)
//   heater    register the heater is read from (default 1)
//   initial   starting temperature, in 0.1 °C (default 180)
//   heat      change per step with the heater on (default 5)
//   cool      change per step with the heater off (default 3)
std::unique_ptr<Plant> make_thermal_plant(const PlantParams& params, const Scenario& scenario);

}  // namespace costar
