#pragma once

#include <cstddef>
#include <cstdint>
#include <variant>
#include <vector>

#include "costar/devices.hpp"
#include "costar/time.hpp"
#include "costar/wires.hpp"

namespace costar {

class World;

// A device model: a part on a bus (a sensor, an EEPROM) that answers the
// firmware's controller.  The world hands it every frame that arrives on its
// endpoint, a channel, and it answers by sending from that endpoint, so
// requests and answers cross wires with their timing and faults.  A model
// keeps its state in devices (usually on a passive machine), where inputs,
// plants, Inspect and the trace see it.
class Model {
 public:
  virtual ~Model() = default;
  virtual void receive(const Frame& frame, World& world) = 0;
};

// A time window [from, until) during which an I2C part NACKs its address,
// as a busy or broken one does.
struct NackWindow {
  Time from = 0;
  Time until = kNever;
};

// An I2C part whose registers are a registers device: register n holds the
// part's register at address n.  A write sets the register address, then
// writes registers from there; a read reads registers from there.  A write
// interrupted within the register address leaves the address as it was.
struct I2cRegistersSpec {
  uint8_t address = 0;  // 7-bit target address
  Endpoint registers;
  uint32_t address_bytes = 1;   // register address width: 1 or 2 bytes, MSB first
  uint32_t register_bytes = 1;  // register width: 1 to 4 bytes
  bool little_endian = false;   // registers' byte order (most parts send the MSB first)
  bool auto_increment = true;   // move to the next register after each one
  std::vector<NackWindow> nacks = {};
};

// A 24xx-style I2C EEPROM on a memory device of its size.  A write sets the
// memory address and then buffers data within one page, wrapping around in
// it; the stop writes the page.  Reads run on through the whole memory.
// Memory addresses wrap around it (a part ignores the address bits beyond
// its size), and a write interrupted within the address leaves the address
// as it was.
struct I2cEepromSpec {
  // Its first address.  A part with more memory than its address bytes
  // reach (24C04 to 24C16) answers the following addresses too, one per
  // block, as Zephyr's atmel,at24 driver expects.
  uint8_t address = 0;
  Endpoint memory;
  uint32_t address_bytes = 1;  // memory address width: 1 or 2 bytes, MSB first
  uint32_t page_size = 8;
  Time write_time = 0;  // after a write it NACKs its address for this long
  std::vector<NackWindow> nacks = {};
};

// The I2C addresses an EEPROM of `memory_size` bytes answers on.
uint32_t i2c_eeprom_address_count(const I2cEepromSpec& spec, size_t memory_size);

// A device model as a scenario describes it: the channel it answers (its
// bus) and the part.  Several parts can share a bus.
struct ModelSpec {
  Endpoint bus;
  std::variant<I2cRegistersSpec, I2cEepromSpec> part;
};

// Build the models and add them to `world`, whose machines must exist: one
// model per bus endpoint, which answers for all of the bus's parts (an I2C
// bus NACKs addresses no part answers).  Throws if two parts answer the
// same address.
void add_models(World& world, const std::vector<ModelSpec>& specs);

}  // namespace costar
