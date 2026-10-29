#pragma once

#include "costar/time.hpp"

namespace costar {

class World;

// A model of the physical world around the machines (a vehicle, a room).
// World::add_plant() steps it periodically; it reads actuators and writes
// sensors through the machines' devices.
class Plant {
 public:
  virtual ~Plant() = default;
  virtual void step(Time now, World& world) = 0;
};

}  // namespace costar
