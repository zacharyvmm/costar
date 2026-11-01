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

// A SPI NOR flash (the common 25-series command set) on a memory device of
// its size, up to 16 MiB (3-byte addresses).  It answers the JEDEC ID,
// status (write in progress, write enable latch), write enable and
// disable, read and fast read, page program (bits only go from 1 to 0),
// 4 KiB sector, 32 and 64 KiB block and chip erase.  Programs and erases
// need write enable, which they clear, and keep it busy for their time;
// while busy it ignores everything but reading status.
struct SpiNorSpec {
  uint8_t chip_select = 0;
  Endpoint memory;
  uint32_t jedec_id = 0;  // manufacturer, memory type, capacity: 0xef4014
  uint32_t page_size = 256;
  Time program_time = 0;  // a page program
  Time erase_time = 0;    // a sector, block or chip erase
};

// A device model as a scenario describes it: the channel it answers (its
// bus) and the part.  Several parts can share a bus (of one kind: I2C or SPI).
struct ModelSpec {
  Endpoint bus;
  std::variant<I2cRegistersSpec, I2cEepromSpec, SpiNorSpec> part;
};

// Build the models and add them to `world`, whose machines must exist: one
// model per bus endpoint, which answers for all of the bus's parts (an I2C
// bus NACKs addresses no part answers; nothing on a SPI chip select reads
// 0xFF).  Throws if two parts answer the same address or chip select, or
// I2C and SPI parts share a bus.
void add_models(World& world, const std::vector<ModelSpec>& specs);

}  // namespace costar
