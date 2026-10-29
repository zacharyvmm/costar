#pragma once

#include <cstddef>
#include <cstdint>
#include <deque>
#include <map>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

namespace costar {

using Frame = std::vector<uint8_t>;

inline constexpr unsigned kNoIrq = ~0u;

// Frames in and out (UART, CAN, Ethernet, SPI, I2C, ...).  Sent frames leave
// through the machine's outbox; received frames wait here.
struct Channel {
  std::deque<Frame> received;
};

// 32-bit values by index (GPIO, ADC, sensors, ...).
struct Registers {
  std::map<uint32_t, uint32_t> values;
};

// Bytes (flash, EEPROM, block devices).  Erased bytes read 0xFF.
struct Memory {
  std::vector<uint8_t> bytes;
};

enum class DeviceKind { Channel, Registers, Memory };

// How the host describes one device of a board.
struct DeviceSpec {
  std::string name;
  DeviceKind kind;
  unsigned irq = kNoIrq;
  size_t size = 0;  // Memory: size in bytes
};

struct Device {
  std::string name;
  unsigned irq;
  std::variant<Channel, Registers, Memory> state;
};

// The devices of one machine, addressed by handle (firmware) or name (host).
class Devices {
 public:
  explicit Devices(const std::vector<DeviceSpec>& specs);

  size_t size() const { return devices_.size(); }
  // Handle of the named device, or -1.
  int find(std::string_view name) const;
  Device* at(int handle);
  const Device& at(int handle) const { return devices_.at(handle); }

  // The device's state if `handle` names a device of shape T, else nullptr.
  template <class T>
  T* get(int handle) {
    Device* device = at(handle);
    return device == nullptr ? nullptr : std::get_if<T>(&device->state);
  }

  // Host-side access by name; throws if there is no such device of shape T.
  template <class T>
  T& get(std::string_view name) {
    T* state = get<T>(find(name));
    if (state == nullptr) throw_missing(name);
    return *state;
  }

 private:
  [[noreturn]] static void throw_missing(std::string_view name);

  std::vector<Device> devices_;
};

}  // namespace costar
