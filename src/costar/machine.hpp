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
//
// A machine starts powered on.  Powering it off loses the CPU's state and the
// image's RAM, and frames queued on its channels; frames that arrive while it
// is off are dropped.  Registers and memory keep their contents (sensors and
// flash).  Powering on boots a fresh copy of the image.
class Machine {
 public:
  Machine(uint32_t id, const std::optional<std::filesystem::path>& image, const std::vector<DeviceSpec>& devices,
          const Time& now, Trace& trace);
  ~Machine();
  Machine(const Machine&) = delete;
  Machine& operator=(const Machine&) = delete;

  uint32_t id() const { return id_; }
  Time now() const { return now_; }
  Cpu& cpu() { return cpu_; }
  const Cpu& cpu() const { return cpu_; }
  bool has_image() const { return image_file_.has_value(); }
  Devices& devices() { return devices_; }

  // ── Power ───────────────────────────────────────────────────────────
  // Only between runs of the CPU (host events, plants, sessions).
  void power_off();
  // A no-op if the machine is on, even if its firmware has stopped (as a
  // board that has halted stays powered): power off first, or reset.
  void power_on();
  bool powered() const { return powered_; }

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
  void boot();
  void raise_irq_of(int dev);
  std::optional<std::span<uint8_t>> memory_range(int dev, uint32_t offset, size_t length);

  uint32_t id_;
  const Time& now_;
  Trace& trace_;
  Devices devices_;
  std::vector<std::pair<int, Frame>> outbox_;
  bool powered_ = false;
  std::optional<std::filesystem::path> image_file_;
  std::optional<Image> image_;  // Declared before cpu_ so the code outlives the threads running it.
  Cpu cpu_;
};

}  // namespace costar
