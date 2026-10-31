#pragma once

#include <cstdint>
#include <filesystem>
#include <map>
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

// Firmware calls into costar at one instant before its machine is stopped as
// stalled.  On an infinitely fast CPU, firmware that keeps running without
// letting time pass (a thread switch or polling loop) would hang the world.
constexpr uint64_t kDefaultStallLimit = 1'000'000;

// What a basic block of instrumented firmware costs, in picoseconds, unless
// the machine says otherwise (costar_cpu_work(); INSTRUMENT, ARCHITECTURE.md §5).
constexpr uint64_t kDefaultBlockPs = 10'000;

// A thread that used more of its stack than its machine's StackCheck allows.
struct StackOverrun {
  Time at;
  uint32_t machine;
  ThreadStack thread;

  bool operator==(const StackOverrun&) const = default;
};

// One simulated board: a CPU running its own copy of a firmware image, and
// the board's devices.  A machine without an image is a passive endpoint whose
// devices only the host drives.
//
// A machine starts powered on.  Powering it off loses the CPU's state and the
// image's RAM, and frames queued on its channels; frames that arrive while it
// is off are dropped.  Registers and memory keep their contents (sensors and
// flash).  Powering on boots a fresh copy of the image.
//
// A watchdog device that firmware has started resets the machine (as power
// off and on, at once) if it isn't fed in time.
class Machine {
 public:
  Machine(uint32_t id, const std::optional<std::filesystem::path>& image, const std::vector<DeviceSpec>& devices,
          const Time& now, Trace& trace);
  ~Machine();
  Machine(const Machine&) = delete;
  Machine& operator=(const Machine&) = delete;

  uint32_t id() const { return id_; }
  Time now() const { return now_; }
  // When this machine next needs the world's attention: its CPU's next event
  // or a watchdog's deadline (kNever if never).
  Time next_event() const;
  Cpu& cpu() { return cpu_; }
  const Cpu& cpu() const { return cpu_; }
  bool has_image() const { return image_file_.has_value(); }
  Devices& devices() { return devices_; }
  const Devices& devices() const { return devices_; }
  // 0: never stop the machine as stalled.
  void set_stall_limit(uint64_t limit) { stall_limit_ = limit; }
  // When the machine was stopped as stalled, oldest first.  Kept apart from
  // the trace: firmware may trace a label of its own named `stall`.
  const std::vector<Time>& stalls() const { return stalls_; }
  // For threads created or described from now on.
  void set_stack_check(const StackCheck& check) { cpu_.set_stack_check(check); }
  // How long a basic block of instrumented firmware takes on this machine's
  // CPU, in picoseconds (at least 1): its speed.
  void set_block_ps(uint64_t picoseconds);
  uint64_t block_ps() const { return block_ps_; }
  // Threads that overran their stacks, oldest first.  The first stopped the
  // CPU, with a `stack` record.
  const std::vector<StackOverrun>& overruns() const { return overruns_; }

  // Run the CPU until it stops being awake.  A thread found to have overrun
  // its stack stops it.
  Cpu::State run();
  // Scan every thread's whole stack, and record overruns that the canaries
  // missed (a thread that jumped over its canary).  Powering off does this.
  void check_stacks();
  // Reset the machine if a watchdog's deadline has come: a `watchdog`
  // record, then off and on.  Between runs of the CPU, as power_off().
  void expire_watchdogs();

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
  // Take the oldest frame received on a channel, as a device model does
  // (nullopt: none is waiting).
  std::optional<Frame> receive(std::string_view device);
  bool has_received(std::string_view device) const;
  // The device transmits a frame, as if the firmware had sent it.
  void send(std::string_view device, Frame frame);
  // The world changes a register (a sensor reading, an input pin).
  void set_register(std::string_view device, uint32_t reg, uint32_t value);
  // Preload or overwrite memory contents.
  void write_memory(std::string_view device, uint32_t offset, std::span<const uint8_t> data);
  // Erase memory (to 0xFF), as a flash part does.
  void erase_memory(std::string_view device, uint32_t offset, size_t length);
  // Frames the firmware sent since the last call, in send order.
  std::vector<std::pair<std::string, Frame>> take_outbox();
  bool has_outbox() const { return !outbox_.empty(); }

  // ── Firmware side (costar.h, via abi.cpp) ───────────────────────────
  // Every call from firmware passes through here first.  Too many at one
  // instant stop the CPU as stalled: a `stall` record, then as costar_cpu_stop().
  void count_call();
  void trace(const char* label, uint32_t value);
  void log(std::string_view text);
  [[noreturn]] void stop(int status);
  // Busy for `blocks` basic blocks at this machine's speed.
  void cpu_work(uint32_t blocks);
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
  // Lose power: the CPU, the image's RAM, running watchdogs, and frames
  // queued on channels, which are recorded as dropped after `why`.
  void shut_down(TraceRecord why);
  void watchdog_write(int dev, uint32_t reg, uint32_t value);
  void raise_irq_of(int dev);
  void record_overrun(const ThreadStack& thread);
  std::optional<std::span<uint8_t>> memory_range(int dev, uint32_t offset, size_t length);

  uint32_t id_;
  const Time& now_;
  Trace& trace_;
  Devices devices_;
  std::vector<std::pair<int, Frame>> outbox_;
  bool powered_ = false;
  uint64_t stall_limit_ = kDefaultStallLimit;
  Time calls_at_ = 0;  // the instant `calls_` counts at
  uint64_t calls_ = 0;
  std::vector<Time> stalls_;
  std::vector<StackOverrun> overruns_;
  uint64_t block_ps_ = kDefaultBlockPs;
  uint64_t work_ps_ = 0;  // charged work not yet a whole nanosecond
  std::map<int, Time> watchdogs_;  // running watchdogs (device handle) and their deadlines
  std::optional<std::filesystem::path> image_file_;
  std::optional<Image> image_;  // Declared before cpu_ so the code outlives the threads running it.
  Cpu cpu_;
};

}  // namespace costar
