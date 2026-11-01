#include "costar/models.hpp"

#include <algorithm>
#include <format>
#include <map>
#include <memory>
#include <optional>
#include <span>
#include <stdexcept>
#include <string>
#include <type_traits>

#include "costar_i2c.h"
#include "costar_spi.h"
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

// ── SPI ───────────────────────────────────────────────────────────────

// One part on a SPI bus, as the bus sees it: a byte each way at a time.
class SpiTarget {
 public:
  virtual ~SpiTarget() = default;
  virtual uint8_t chip_select() const = 0;
  // Chip select goes active: a transaction starts.
  virtual void select(World& world) = 0;
  // One byte out (MOSI), one byte back (MISO).
  virtual uint8_t exchange(uint8_t mosi, World& world) = 0;
  // Chip select is released: the part acts on a command that needs it.
  virtual void deselect(World& world) = 0;
};

class SpiNor final : public SpiTarget {
 public:
  SpiNor(SpiNorSpec spec, size_t size) : spec_(std::move(spec)), size_(size) {}

  uint8_t chip_select() const override { return spec_.chip_select; }

  void select(World&) override {
    opcode_.reset();
    count_ = 0;
    address_ = 0;
    pending_.clear();
  }

  uint8_t exchange(uint8_t mosi, World& world) override {
    if (!opcode_) {
      opcode_ = mosi;
      return 0xff;
    }
    const size_t index = count_++;  // bytes after the opcode
    if (busy(world) && *opcode_ != kReadStatus) return 0xff;
    switch (*opcode_) {
      case kReadId: return index < 3 ? static_cast<uint8_t>(spec_.jedec_id >> 8 * (2 - index)) : 0;
      case kReadStatus: return status(world);
      case kRead:
      case kFastRead:
      case kPageProgram:
      case kSectorErase:
      case kBlockErase32:
      case kBlockErase64: break;
      default: return 0xff;
    }
    if (index < 3) {  // the address, MSB first
      address_ = (address_ << 8 | mosi) % size_;
      return 0xff;
    }
    switch (*opcode_) {
      case kRead:
      case kFastRead: {
        if (*opcode_ == kFastRead && index == 3) return 0xff;  // a dummy byte
        const uint8_t byte = memory(world)[address_];
        address_ = (address_ + 1) % size_;
        return byte;
      }
      case kPageProgram: {
        pending_[address_] = mosi;
        const size_t page = address_ - address_ % spec_.page_size;
        address_ = address_ + 1 == std::min(page + spec_.page_size, size_) ? page : address_ + 1;
        return 0xff;
      }
      default: return 0xff;
    }
  }

  void deselect(World& world) override {
    if (!opcode_ || busy(world)) return;  // a busy part ignores commands
    switch (*opcode_) {
      case kWriteEnable: write_enabled_ = true; break;
      case kWriteDisable: write_enabled_ = false; break;
      case kPageProgram:
        if (count_ > 3 && take_write_enable()) program(world);
        break;
      case kSectorErase: erase(world, 4096, count_ == 3); break;
      case kBlockErase32: erase(world, 32768, count_ == 3); break;
      case kBlockErase64: erase(world, 65536, count_ == 3); break;
      case kChipErase:
      case kChipEraseAlt:
        address_ = 0;
        erase(world, size_, count_ == 0);
        break;
      case kWriteStatus: take_write_enable(); break;  // no protection bits are modelled
      default: break;
    }
  }

 private:
  static constexpr uint8_t kWriteStatus = 0x01, kPageProgram = 0x02, kRead = 0x03, kWriteDisable = 0x04,
                           kReadStatus = 0x05, kWriteEnable = 0x06, kFastRead = 0x0b, kSectorErase = 0x20,
                           kBlockErase32 = 0x52, kChipEraseAlt = 0x60, kReadId = 0x9f, kChipErase = 0xc7,
                           kBlockErase64 = 0xd8;

  bool busy(World& world) const { return world.now() < busy_until_; }
  uint8_t status(World& world) const { return (busy(world) ? 0x01 : 0) | (write_enabled_ ? 0x02 : 0); }

  bool take_write_enable() {
    const bool enabled = write_enabled_;
    write_enabled_ = false;
    return enabled;
  }

  std::span<const uint8_t> memory(World& world) const {
    return world.machine(spec_.memory.machine).devices().get<Memory>(spec_.memory.device).bytes;
  }

  // Program the page buffer: bits only go from 1 to 0.  One `mem` record
  // per run of consecutive bytes.
  void program(World& world) {
    const std::span<const uint8_t> memory = this->memory(world);
    Machine& machine = world.machine(spec_.memory.machine);
    for (auto it = pending_.begin(); it != pending_.end();) {
      const size_t offset = it->first;
      Frame run;
      for (; it != pending_.end() && it->first == offset + run.size(); ++it) {
        run.push_back(memory[it->first] & it->second);
      }
      machine.write_memory(spec_.memory.device, static_cast<uint32_t>(offset), run);
    }
    busy_until_ = time_add(world.now(), spec_.program_time);
  }

  // Erase the `size`-byte region around the address, if the command was
  // complete (`whole`: CS released right after its address).
  void erase(World& world, size_t size, bool whole) {
    if (!whole || !take_write_enable()) return;
    const size_t start = address_ - address_ % size;
    world.machine(spec_.memory.machine)
        .erase_memory(spec_.memory.device, static_cast<uint32_t>(start), std::min(size, size_ - start));
    busy_until_ = time_add(world.now(), spec_.erase_time);
  }

  SpiNorSpec spec_;
  size_t size_;
  std::optional<uint8_t> opcode_;  // the transaction's command
  size_t count_ = 0;               // bytes after the opcode so far
  size_t address_ = 0;
  std::map<size_t, uint8_t> pending_;  // a page program's data: offset → byte
  bool write_enabled_ = false;
  Time busy_until_ = 0;
};

// The parts on one SPI bus endpoint (include/costar_spi.h).
class SpiBus final : public Model {
 public:
  explicit SpiBus(Endpoint endpoint) : endpoint_(std::move(endpoint)) {}

  void add(std::unique_ptr<SpiTarget> target) {
    if (find(target->chip_select()) != nullptr) {
      throw std::invalid_argument(
          std::format("two SPI models answer chip select {} on {}", target->chip_select(), endpoint_.device));
    }
    targets_.push_back(std::move(target));
  }

  void receive(const Frame& request, World& world) override {
    if (request.size() < 2) return;  // nothing to answer
    const uint8_t chip_select = request[0];
    Frame reply{chip_select, request[1], COSTAR_SPI_BAD_REQUEST};
    if (request.size() >= COSTAR_SPI_REQUEST_HEADER) {
      const uint8_t flags = request[2];
      const size_t length = request[3] << 8 | request[4];
      const size_t wanted = request[5] << 8 | request[6];
      const std::span<const uint8_t> out = std::span(request).subspan(COSTAR_SPI_REQUEST_HEADER);
      if ((flags & ~(COSTAR_SPI_SELECT | COSTAR_SPI_DESELECT)) == 0 && wanted <= length && out.size() <= length) {
        reply[2] = COSTAR_SPI_OK;
        transfer(chip_select, flags, length, wanted, out, reply, world);
      }
    }
    world.machine(endpoint_.machine).send(endpoint_.device, std::move(reply));
  }

 private:
  SpiTarget* find(uint8_t chip_select) {
    for (const auto& target : targets_) {
      if (target->chip_select() == chip_select) return target.get();
    }
    return nullptr;
  }

  void transfer(uint8_t chip_select, uint8_t flags, size_t length, size_t wanted, std::span<const uint8_t> out,
                Frame& reply, World& world) {
    if (selected_ && (*selected_ != chip_select || (flags & COSTAR_SPI_SELECT) != 0)) release(world);
    SpiTarget* part = find(chip_select);
    if (!selected_) {
      selected_ = chip_select;
      if (part != nullptr) part->select(world);
    }
    for (size_t i = 0; i < length; ++i) {
      const uint8_t miso = part != nullptr ? part->exchange(i < out.size() ? out[i] : 0, world) : 0xff;
      if (i < wanted) reply.push_back(miso);
    }
    if ((flags & COSTAR_SPI_DESELECT) != 0) release(world);
  }

  void release(World& world) {
    if (SpiTarget* part = find(*selected_)) part->deselect(world);
    selected_.reset();
  }

  Endpoint endpoint_;
  std::vector<std::unique_ptr<SpiTarget>> targets_;
  std::optional<uint8_t> selected_;  // the chip select that is active
};

}  // namespace

uint32_t i2c_eeprom_address_count(const I2cEepromSpec& spec, size_t memory_size) {
  const size_t block = size_t{1} << 8 * spec.address_bytes;
  return static_cast<uint32_t>((memory_size + block - 1) / block);
}

void add_models(World& world, const std::vector<ModelSpec>& specs) {
  std::vector<std::pair<Endpoint, std::unique_ptr<Model>>> buses;
  // The bus model on `endpoint`, of type B.
  const auto bus = [&]<class B>(const Endpoint& endpoint, std::type_identity<B>) -> B& {
    for (auto& [other, model] : buses) {
      if (other != endpoint) continue;
      B* bus = dynamic_cast<B*>(model.get());
      if (bus == nullptr) throw std::invalid_argument("I2C and SPI models share channel " + endpoint.device);
      return *bus;
    }
    return static_cast<B&>(*buses.emplace_back(endpoint, std::make_unique<B>(endpoint)).second);
  };
  const auto memory_size = [&](const Endpoint& memory) {
    const size_t size = world.machine(memory.machine).devices().get<Memory>(memory.device).bytes.size();
    if (size == 0) throw std::invalid_argument("a model's memory device " + memory.device + " is empty");
    return size;
  };
  for (const ModelSpec& spec : specs) {
    if (const auto* part = std::get_if<I2cRegistersSpec>(&spec.part)) {
      world.machine(part->registers.machine).devices().get<Registers>(part->registers.device);  // throws if missing
      bus(spec.bus, std::type_identity<I2cBus>{}).add(std::make_unique<I2cRegisters>(*part), part->nacks);
    } else if (const auto* part = std::get_if<I2cEepromSpec>(&spec.part)) {
      if (part->page_size == 0) throw std::invalid_argument("an I2C EEPROM needs a page size");
      bus(spec.bus, std::type_identity<I2cBus>{})
          .add(std::make_unique<I2cEeprom>(*part, memory_size(part->memory)), part->nacks);
    } else if (const auto* part = std::get_if<SpiNorSpec>(&spec.part)) {
      const size_t size = memory_size(part->memory);
      if (size > size_t{1} << 24 || part->page_size == 0) {
        throw std::invalid_argument("a SPI NOR flash needs a page size and at most 16 MiB");
      }
      bus(spec.bus, std::type_identity<SpiBus>{}).add(std::make_unique<SpiNor>(*part, size));
    }
  }
  for (auto& [endpoint, model] : buses) world.add_model(endpoint, std::move(model));
}

}  // namespace costar
