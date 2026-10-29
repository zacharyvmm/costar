#pragma once

#include <cstdint>
#include <filesystem>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "costar/cpu.hpp"
#include "costar/devices.hpp"
#include "costar/image.hpp"
#include "costar/time.hpp"
#include "costar/trace.hpp"

namespace costar {

// One simulated board: a CPU running its own copy of a firmware image, and
// the board's devices.  A machine without an image is a passive endpoint whose
// devices only the host drives.
class Machine {
 public:
  Machine(uint32_t id, const std::optional<std::filesystem::path>& image, const std::vector<DeviceSpec>& devices,
          const Time& now, Trace& trace);
  Machine(const Machine&) = delete;
  Machine& operator=(const Machine&) = delete;

  uint32_t id() const { return id_; }
  Time now() const { return now_; }
  Cpu& cpu() { return cpu_; }
  Devices& devices() { return devices_; }

  // ── Host side ───────────────────────────────────────────────────────
  // A frame arrives on a channel: queue it and raise the device's IRQ.
  void deliver(std::string_view device, Frame frame);
  // The device transmits a frame, as if the firmware had sent it.
  void send(std::string_view device, Frame frame);
  // The world changes a register (a sensor reading, an input pin).
  void set_register(std::string_view device, uint32_t reg, uint32_t value);
  // Preload or overwrite memory contents.
  void write_memory(std::string_view device, uint32_t offset, std::span<const uint8_t> data);
  // Frames the firmware sent since the last call, in send order.
  std::vector<std::pair<std::string, Frame>> take_outbox();
  bool has_outbox() const { return !outbox_.empty(); }

  // ── Firmware side (costar.h, via abi.cpp) ───────────────────────────
  void trace(const char* label, uint32_t value);
  void log(std::string_view text);
  [[noreturn]] void stop(int status);
  int dev_irq(int dev);
  int chan_send(int dev, std::span<const uint8_t> frame);
  int chan_recv(int dev, std::span<uint8_t> buffer);
  uint32_t reg_read(int dev, uint32_t reg);
  void reg_write(int dev, uint32_t reg, uint32_t value);
  size_t mem_size(int dev);
  int mem_read(int dev, uint32_t offset, std::span<uint8_t> buffer);
  int mem_write(int dev, uint32_t offset, std::span<const uint8_t> data);
  int mem_erase(int dev, uint32_t offset, size_t length);

 private:
  void raise_irq_of(int dev);
  std::optional<std::span<uint8_t>> memory_range(int dev, uint32_t offset, size_t length);

  uint32_t id_;
  const Time& now_;
  Trace& trace_;
  Devices devices_;
  std::vector<std::pair<int, Frame>> outbox_;
  std::optional<Image> image_;  // Declared before cpu_ so the code outlives the threads running it.
  Cpu cpu_;
};

}  // namespace costar
