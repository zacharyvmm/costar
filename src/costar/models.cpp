#include "costar/models.hpp"

#include <algorithm>
#include <format>
#include <map>
#include <memory>
#include <optional>
#include <span>
#include <stdexcept>
#include <string>

#include "costar_i2c.h"
#include "costar/world.hpp"

namespace costar {

namespace {

bool in_window(const std::vector<NackWindow>& windows, Time now) {
  return std::ranges::any_of(windows, [now](const NackWindow& w) { return now >= w.from && now < w.until; });
}

// ── I2C ───────────────────────────────────────────────────────────────

// One part on an I2C bus, as the bus sees it: a byte at a time.
class I2cTarget {
 public:
  virtual ~I2cTarget() = default;
  virtual bool answers(uint16_t address) const = 0;
  // A start or repeated start addressed to the part; false: it NACKs.
  virtual bool start(uint16_t address, bool read, World& world) = 0;
  // A byte written to the part; false: it NACKs.
  virtual bool write(uint8_t byte, World& world) = 0;
  virtual uint8_t read(World& world) = 0;
  // The transaction ends: a stop, or the controller gave up after a NACK.
  virtual void stop(World& world) = 0;
};

class I2cRegisters final : public I2cTarget {
 public:
  explicit I2cRegisters(I2cRegistersSpec spec) : spec_(std::move(spec)) {}

  bool answers(uint16_t address) const override { return address == spec_.address; }

  bool start(uint16_t, bool read, World& world) override {
    commit(world);  // a repeated start ends a partly written register
    lane_ = 0;
    address_left_ = read ? 0 : spec_.address_bytes;
    address_ = 0;
    return true;
  }

  bool write(uint8_t byte, World& world) override {
    if (address_left_ > 0) {
      // Only a whole register address moves the pointer: a part interrupted
      // within one (a repeated start, a stop) keeps the last.
      address_ = address_ << 8 | byte;
      if (--address_left_ == 0) pointer_ = address_;
      return true;
    }
    if (!pending_) pending_ = value(world);
    const uint32_t shift = this->shift(lane_);
    *pending_ = (*pending_ & ~(0xffu << shift)) | uint32_t{byte} << shift;
    if (++lane_ == spec_.register_bytes) {
      commit(world);
      next();
    }
    return true;
  }

  uint8_t read(World& world) override {
    const auto byte = static_cast<uint8_t>(value(world) >> shift(lane_));
    if (++lane_ == spec_.register_bytes) next();
    return byte;
  }

  void stop(World& world) override {
    commit(world);
    lane_ = 0;
    address_left_ = 0;
  }

 private:
  uint32_t shift(uint32_t lane) const { return 8 * (spec_.little_endian ? lane : spec_.register_bytes - 1 - lane); }

  uint32_t value(World& world) const {
    const auto& values = world.machine(spec_.registers.machine).devices().get<Registers>(spec_.registers.device).values;
    const auto it = values.find(pointer_);
    const uint32_t mask = spec_.register_bytes == 4 ? ~0u : (1u << 8 * spec_.register_bytes) - 1;
    return it == values.end() ? 0 : it->second & mask;
  }

  void commit(World& world) {
    if (!pending_) return;
    world.machine(spec_.registers.machine).set_register(spec_.registers.device, pointer_, *pending_);
    pending_.reset();
  }

  void next() {
    lane_ = 0;
    if (spec_.auto_increment) pointer_ = (pointer_ + 1) & ((1u << 8 * spec_.address_bytes) - 1);
  }

  I2cRegistersSpec spec_;
  uint32_t pointer_ = 0;       // the register address
  uint32_t address_ = 0;       // a register address being written
  uint32_t address_left_ = 0;  // its bytes still to come in this write
  uint32_t lane_ = 0;          // the byte within the register
  std::optional<uint32_t> pending_;  // the register being written
};

class I2cEeprom final : public I2cTarget {
 public:
  I2cEeprom(I2cEepromSpec spec, size_t size)
      : spec_(std::move(spec)), size_(size), count_(i2c_eeprom_address_count(spec_, size)) {}

  bool answers(uint16_t address) const override {
    return address >= spec_.address && address < spec_.address + count_;
  }

  bool start(uint16_t address, bool read, World& world) override {
    pending_.clear();  // a write not ended by a stop is abandoned
    if (world.now() < busy_until_) return false;
    address_left_ = read ? 0 : spec_.address_bytes;
    // The block its address selects: the memory address's high bits.
    address_ = static_cast<size_t>(address - spec_.address) << 8 * spec_.address_bytes;
    return true;
  }

  bool write(uint8_t byte, World&) override {
    if (address_left_ > 0) {
      // Only a whole memory address moves the pointer (a write without one,
      // or interrupted within one, leaves it), and it wraps around the
      // memory, as a part ignores the address bits beyond its size.
      address_ |= size_t{byte} << 8 * --address_left_;
      if (address_left_ == 0) pointer_ = address_ % size_;
      return true;
    }
    pending_[pointer_] = byte;
    const size_t page = pointer_ - pointer_ % spec_.page_size;
    const size_t end = std::min(page + spec_.page_size, size_);
    pointer_ = pointer_ + 1 == end ? page : pointer_ + 1;
    return true;
  }

  uint8_t read(World& world) override {
    const uint8_t byte = memory(world)[pointer_];
    pointer_ = (pointer_ + 1) % size_;
    return byte;
  }

  void stop(World& world) override {
    if (pending_.empty()) return;
    // One `mem` record per run of consecutive bytes.
    Machine& machine = world.machine(spec_.memory.machine);
    for (auto it = pending_.begin(); it != pending_.end();) {
      const size_t offset = it->first;
      Frame run;
      while (it != pending_.end() && it->first == offset + run.size()) run.push_back((it++)->second);
      machine.write_memory(spec_.memory.device, static_cast<uint32_t>(offset), run);
    }
    pending_.clear();
    busy_until_ = time_add(world.now(), spec_.write_time);
  }

 private:
  std::span<const uint8_t> memory(World& world) const {
    return world.machine(spec_.memory.machine).devices().get<Memory>(spec_.memory.device).bytes;
  }

  I2cEepromSpec spec_;
  size_t size_;
  uint32_t count_;
  size_t pointer_ = 0;         // the memory address: always within it
  size_t address_ = 0;         // a memory address being written
  uint32_t address_left_ = 0;  // its bytes still to come in this write
  std::map<size_t, uint8_t> pending_;  // the page being written: offset → byte
  Time busy_until_ = 0;
};

// The controller's side of one transfer.
struct I2cMessage {
  uint8_t flags;
  uint16_t length;
  std::span<const uint8_t> data;  // a write's
};

std::optional<std::vector<I2cMessage>> parse_i2c_messages(std::span<const uint8_t> bytes) {
  std::vector<I2cMessage> messages;
  while (!bytes.empty()) {
    if (bytes.size() < COSTAR_I2C_MESSAGE_HEADER) return std::nullopt;
    I2cMessage m{bytes[0], static_cast<uint16_t>(bytes[1] << 8 | bytes[2]), {}};
    bytes = bytes.subspan(COSTAR_I2C_MESSAGE_HEADER);
    if ((m.flags & ~(COSTAR_I2C_MSG_READ | COSTAR_I2C_MSG_STOP | COSTAR_I2C_MSG_RESTART)) != 0) return std::nullopt;
    if ((m.flags & COSTAR_I2C_MSG_READ) == 0) {
      if (bytes.size() < m.length) return std::nullopt;
      m.data = bytes.first(m.length);
      bytes = bytes.subspan(m.length);
    }
    messages.push_back(m);
  }
  if (messages.empty()) return std::nullopt;
  return messages;
}

// The parts on one I2C bus endpoint (include/costar_i2c.h).
class I2cBus final : public Model {
 public:
  explicit I2cBus(Endpoint endpoint) : endpoint_(std::move(endpoint)) {}

  void add(std::unique_ptr<I2cTarget> target, std::vector<NackWindow> nacks) {
    for (uint16_t address = 0; address < 0x80; ++address) {
      if (!target->answers(address)) continue;
      if (find(address) != nullptr) {
        throw std::invalid_argument(std::format("two I2C models answer address {:#04x} on {}", address, endpoint_.device));
      }
    }
    targets_.push_back({std::move(target), std::move(nacks)});
  }

  void receive(const Frame& request, World& world) override {
    if (request.size() < COSTAR_I2C_REQUEST_HEADER) return;  // nothing to answer
    Frame reply{request[0], request[1], request[2], COSTAR_I2C_BAD_REQUEST};
    const auto address = static_cast<uint16_t>(request[0] << 8 | request[1]);
    if (const auto messages = parse_i2c_messages(std::span(request).subspan(COSTAR_I2C_REQUEST_HEADER))) {
      Frame data;
      reply[3] = transfer(address, *messages, data, world);
      if (reply[3] == COSTAR_I2C_ACK) reply.insert(reply.end(), data.begin(), data.end());
    }
    world.machine(endpoint_.machine).send(endpoint_.device, std::move(reply));
  }

 private:
  struct Target {
    std::unique_ptr<I2cTarget> part;
    std::vector<NackWindow> nacks;
  };

  Target* find(uint16_t address) {
    for (Target& target : targets_) {
      if (target.part->answers(address)) return &target;
    }
    return nullptr;
  }

  // Run the messages on the bus; the status for the reply.
  uint8_t transfer(uint16_t address, const std::vector<I2cMessage>& messages, Frame& data, World& world) {
    Target* target = (address & COSTAR_I2C_ADDR_10_BITS) != 0 ? nullptr : find(address);
    if (target == nullptr || in_window(target->nacks, world.now())) return COSTAR_I2C_NACK_ADDRESS;
    I2cTarget& part = *target->part;
    bool open = false;  // within a transaction: after a start, before its stop
    bool reading = false;
    for (const I2cMessage& m : messages) {
      const bool read = (m.flags & COSTAR_I2C_MSG_READ) != 0;
      if (!open || (m.flags & COSTAR_I2C_MSG_RESTART) != 0 || read != reading) {
        if (!part.start(address, read, world)) {
          if (open) part.stop(world);
          return COSTAR_I2C_NACK_ADDRESS;
        }
        open = true;
        reading = read;
      }
      if (read) {
        for (uint16_t i = 0; i < m.length; ++i) data.push_back(part.read(world));
      } else {
        for (const uint8_t byte : m.data) {
          if (!part.write(byte, world)) {
            part.stop(world);
            return COSTAR_I2C_NACK_DATA;
          }
        }
      }
      if ((m.flags & COSTAR_I2C_MSG_STOP) != 0) {
        part.stop(world);
        open = false;
      }
    }
    if (open) part.stop(world);
    return COSTAR_I2C_ACK;
  }

  Endpoint endpoint_;
  std::vector<Target> targets_;
};

}  // namespace

uint32_t i2c_eeprom_address_count(const I2cEepromSpec& spec, size_t memory_size) {
  const size_t block = size_t{1} << 8 * spec.address_bytes;
  return static_cast<uint32_t>((memory_size + block - 1) / block);
}

void add_models(World& world, const std::vector<ModelSpec>& specs) {
  std::vector<std::pair<Endpoint, std::unique_ptr<I2cBus>>> buses;
  const auto bus = [&](const Endpoint& endpoint) -> I2cBus& {
    for (auto& [other, model] : buses) {
      if (other == endpoint) return *model;
    }
    return *buses.emplace_back(endpoint, std::make_unique<I2cBus>(endpoint)).second;
  };
  for (const ModelSpec& spec : specs) {
    if (const auto* part = std::get_if<I2cRegistersSpec>(&spec.part)) {
      world.machine(part->registers.machine).devices().get<Registers>(part->registers.device);  // throws if missing
      bus(spec.bus).add(std::make_unique<I2cRegisters>(*part), part->nacks);
    } else if (const auto* part = std::get_if<I2cEepromSpec>(&spec.part)) {
      const size_t size = world.machine(part->memory.machine).devices().get<Memory>(part->memory.device).bytes.size();
      if (size == 0 || part->page_size == 0) throw std::invalid_argument("an I2C EEPROM needs memory and a page size");
      bus(spec.bus).add(std::make_unique<I2cEeprom>(*part, size), part->nacks);
    }
  }
  for (auto& [endpoint, model] : buses) world.add_model(endpoint, std::move(model));
}

}  // namespace costar
