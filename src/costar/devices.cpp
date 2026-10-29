#include "costar/devices.hpp"

#include <stdexcept>

namespace costar {

Devices::Devices(const std::vector<DeviceSpec>& specs) {
  for (const DeviceSpec& spec : specs) {
    if (find(spec.name) >= 0) throw std::invalid_argument("duplicate device name: " + spec.name);
    Device device{spec.name, spec.irq, Channel{}};
    switch (spec.kind) {
      case DeviceKind::Channel: break;
      case DeviceKind::Registers: device.state = Registers{}; break;
      case DeviceKind::Memory: device.state = Memory{Frame(spec.size, 0xFF)}; break;
    }
    devices_.push_back(std::move(device));
  }
}

int Devices::find(std::string_view name) const {
  for (size_t i = 0; i < devices_.size(); ++i) {
    if (devices_[i].name == name) return static_cast<int>(i);
  }
  return -1;
}

Device* Devices::at(int handle) {
  if (handle < 0 || static_cast<size_t>(handle) >= devices_.size()) return nullptr;
  return &devices_[handle];
}

void Devices::throw_missing(std::string_view name) {
  throw std::invalid_argument("no device of that kind named " + std::string(name));
}

}  // namespace costar
