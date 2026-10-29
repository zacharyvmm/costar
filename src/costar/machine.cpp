#include "costar/machine.hpp"

#include <algorithm>
#include <format>

#include "costar/abi.hpp"

namespace costar {

namespace {

void run_reset_vector(void* reset) { reinterpret_cast<costar_image_reset_fn>(reset)(); }

std::string frame_text(std::string_view device, const Frame& frame) {
  std::string text(device);
  text += ' ';
  for (uint8_t byte : frame) text += std::format("{:02x}", byte);
  return text;
}

}  // namespace

Machine::Machine(uint32_t id, const std::optional<std::filesystem::path>& image,
                 const std::vector<DeviceSpec>& devices, const Time& now, Trace& trace)
    : id_(id), now_(now), trace_(trace), devices_(devices), cpu_(now) {
  if (!image) return;
  image_.emplace(*image);
  image_->attach(host_table(), this);
  cpu_.boot(run_reset_vector, reinterpret_cast<void*>(image_->reset()));
}

// ── Host side ─────────────────────────────────────────────────────────

void Machine::deliver(std::string_view device, Frame frame) {
  trace_.record({now_, id_, "rx", static_cast<uint32_t>(frame.size()), frame_text(device, frame)});
  devices_.get<Channel>(device).received.push_back(std::move(frame));
  raise_irq_of(devices_.find(device));
}

void Machine::set_register(std::string_view device, uint32_t reg, uint32_t value) {
  uint32_t& current = devices_.get<Registers>(device).values[reg];
  if (current == value) return;
  current = value;
  raise_irq_of(devices_.find(device));
}

std::vector<std::pair<std::string, Frame>> Machine::take_outbox() {
  std::vector<std::pair<std::string, Frame>> out;
  for (auto& [dev, frame] : outbox_) {
    const std::string& name = devices_.at(dev)->name;
    trace_.record({now_, id_, "tx", static_cast<uint32_t>(frame.size()), frame_text(name, frame)});
    out.emplace_back(name, std::move(frame));
  }
  outbox_.clear();
  return out;
}

void Machine::raise_irq_of(int dev) {
  const unsigned irq = devices_.at(dev)->irq;
  if (irq != kNoIrq) cpu_.raise_irq(irq);
}

// ── Firmware side ─────────────────────────────────────────────────────

void Machine::trace(const char* label, uint32_t value) { trace_.record({now_, id_, label, value}); }

void Machine::log(std::string_view text) {
  if (text.ends_with('\n')) text.remove_suffix(1);
  trace_.record({now_, id_, "log", 0, std::string(text)});
}

void Machine::stop(int status) {
  trace_.record({now_, id_, "stop", static_cast<uint32_t>(status)});
  cpu_.stop();
}

int Machine::dev_irq(int dev) {
  const Device* device = devices_.at(dev);
  return device == nullptr || device->irq == kNoIrq ? -1 : static_cast<int>(device->irq);
}

int Machine::chan_send(int dev, std::span<const uint8_t> frame) {
  if (devices_.get<Channel>(dev) == nullptr) return -1;
  outbox_.emplace_back(dev, Frame(frame.begin(), frame.end()));
  return 0;
}

int Machine::chan_recv(int dev, std::span<uint8_t> buffer) {
  Channel* channel = devices_.get<Channel>(dev);
  if (channel == nullptr || channel->received.empty()) return -1;
  const Frame frame = std::move(channel->received.front());
  channel->received.pop_front();
  std::copy_n(frame.begin(), std::min(frame.size(), buffer.size()), buffer.begin());
  return static_cast<int>(frame.size());
}

uint32_t Machine::reg_read(int dev, uint32_t reg) {
  const Registers* registers = devices_.get<Registers>(dev);
  if (registers == nullptr) return 0;
  const auto it = registers->values.find(reg);
  return it == registers->values.end() ? 0 : it->second;
}

void Machine::reg_write(int dev, uint32_t reg, uint32_t value) {
  Registers* registers = devices_.get<Registers>(dev);
  if (registers == nullptr) return;
  registers->values[reg] = value;
  trace_.record({now_, id_, "reg", value, std::format("{}[{}]", devices_.at(dev)->name, reg)});
}

size_t Machine::mem_size(int dev) {
  const Memory* memory = devices_.get<Memory>(dev);
  return memory == nullptr ? 0 : memory->bytes.size();
}

std::optional<std::span<uint8_t>> Machine::memory_range(int dev, uint32_t offset, size_t length) {
  Memory* memory = devices_.get<Memory>(dev);
  if (memory == nullptr || offset > memory->bytes.size() || length > memory->bytes.size() - offset) {
    return std::nullopt;
  }
  return std::span(memory->bytes).subspan(offset, length);
}

int Machine::mem_read(int dev, uint32_t offset, std::span<uint8_t> buffer) {
  const auto range = memory_range(dev, offset, buffer.size());
  if (!range) return -1;
  std::ranges::copy(*range, buffer.begin());
  return 0;
}

int Machine::mem_write(int dev, uint32_t offset, std::span<const uint8_t> data) {
  const auto range = memory_range(dev, offset, data.size());
  if (!range) return -1;
  std::ranges::copy(data, range->begin());
  return 0;
}

int Machine::mem_erase(int dev, uint32_t offset, size_t length) {
  const auto range = memory_range(dev, offset, length);
  if (!range) return -1;
  std::ranges::fill(*range, 0xFF);
  return 0;
}

}  // namespace costar
