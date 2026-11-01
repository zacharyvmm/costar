#include <doctest/doctest.h>

#include <filesystem>
#include <string>
#include <vector>

#include "costar_i2c.h"
#include "costar/scenario.hpp"
#include "costar/world.hpp"

using costar::Frame;
using costar::milliseconds;

namespace {

// A controller (a passive machine here, standing in for firmware) and a
// board with an I2C bus, a sensor's registers and an EEPROM's memory.
const std::string kBoard = R"(
[[machine]]
name = "mcu"
devices = [{ name = "i2c", kind = "channel" }]

[[machine]]
name = "board"
devices = [{ name = "i2c", kind = "channel" },
           { name = "sensor", kind = "registers" },
           { name = "eeprom", kind = "memory", size = 512 }]

[[wire]]
endpoints = ["mcu.i2c", "board.i2c"]
latency_us = 10
)";

const std::string kSensor = R"(
[[model]]
type = "i2c-registers"
bus = "board.i2c"
address = 0x48
registers = "board.sensor"
register_bytes = 2
)";

const std::string kEeprom = R"(
[[model]]
type = "i2c-eeprom"
bus = "board.i2c"
address = 0x50
memory = "board.eeprom"
page_size = 8
write_time_ms = 2
)";

struct Message {
  uint8_t flags;
  Frame data = {};      // a write's
  uint16_t length = 0;  // a read's
};

Frame request(uint16_t address, uint8_t tag, const std::vector<Message>& messages) {
  Frame out{static_cast<uint8_t>(address >> 8), static_cast<uint8_t>(address), tag};
  for (const Message& m : messages) {
    const size_t length = (m.flags & COSTAR_I2C_MSG_READ) != 0 ? m.length : m.data.size();
    out.insert(out.end(), {m.flags, static_cast<uint8_t>(length >> 8), static_cast<uint8_t>(length)});
    out.insert(out.end(), m.data.begin(), m.data.end());
  }
  return out;
}

Message write(Frame data, uint8_t flags = 0) { return {flags, std::move(data)}; }
Message read(uint16_t length, uint8_t flags = COSTAR_I2C_MSG_RESTART | COSTAR_I2C_MSG_STOP) {
  return {static_cast<uint8_t>(COSTAR_I2C_MSG_READ | flags), {}, length};
}

class Bus {
 public:
  explicit Bus(const std::string& models, const std::string& extra = "") {
    costar::build_world(costar::parse_scenario(kBoard + models + extra, "test.toml"), world);
  }

  // Send a request from the controller and wait for the reply.
  Frame transfer(const Frame& frame) {
    world.machine(0).send("i2c", frame);
    world.run();
    std::optional<Frame> reply = world.machine(0).receive("i2c");
    REQUIRE(reply.has_value());
    CHECK_FALSE(world.machine(0).receive("i2c").has_value());
    return *reply;
  }

  uint32_t reg(uint32_t index) { return world.machine(1).devices().get<costar::Registers>("sensor").values[index]; }
  std::vector<uint8_t>& memory() { return world.machine(1).devices().get<costar::Memory>("eeprom").bytes; }

  costar::World world;
};

Frame reply(uint16_t address, uint8_t tag, uint8_t status, Frame data = {}) {
  Frame out{static_cast<uint8_t>(address >> 8), static_cast<uint8_t>(address), tag, status};
  out.insert(out.end(), data.begin(), data.end());
  return out;
}

}  // namespace

TEST_CASE("an I2C bus NACKs addresses no model answers") {
  Bus bus(kSensor);
  CHECK(bus.transfer(request(0x49, 7, {write({0x00}, COSTAR_I2C_MSG_STOP)})) == reply(0x49, 7, COSTAR_I2C_NACK_ADDRESS));
  CHECK(bus.transfer(request(0x48 | COSTAR_I2C_ADDR_10_BITS, 1, {read(1)})) ==
        reply(0x48 | COSTAR_I2C_ADDR_10_BITS, 1, COSTAR_I2C_NACK_ADDRESS));
  // Malformed: a write shorter than its length, unknown flags, no messages.
  for (const Frame& bad : {Frame{0x00, 0x48, 2, 0x00, 0x00, 0x02, 0xaa}, Frame{0x00, 0x48, 2, 0x10, 0x00, 0x00},
                           Frame{0x00, 0x48, 2}}) {
    CHECK(bus.transfer(bad) == reply(0x48, 2, COSTAR_I2C_BAD_REQUEST));
  }
  CHECK(bus.reg(0) == 0);
}

TEST_CASE("an i2c-registers model writes and reads registers through its register address") {
  Bus bus(kSensor);
  // Write register 1 (two bytes, MSB first), then read it back after a repeated start.
  CHECK(bus.transfer(request(0x48, 1, {write({0x01, 0x12, 0x34}, COSTAR_I2C_MSG_STOP)})) == reply(0x48, 1, 0));
  CHECK(bus.reg(1) == 0x1234);
  CHECK(bus.transfer(request(0x48, 2, {write({0x01}), read(2)})) == reply(0x48, 2, 0, {0x12, 0x34}));

  // The host sets a reading; registers auto-increment, across messages too
  // (a burst write: register address, then data, without a restart).
  bus.world.machine(1).set_register("sensor", 0, 0xabcd);
  CHECK(bus.transfer(request(0x48, 3, {write({0x00}), read(6)})) == reply(0x48, 3, 0, {0xab, 0xcd, 0x12, 0x34, 0, 0}));
  CHECK(bus.transfer(request(0x48, 4, {write({0x02}), write({0x00, 0x05, 0x00, 0x06}, COSTAR_I2C_MSG_STOP)})) ==
        reply(0x48, 4, 0));
  CHECK(bus.reg(2) == 5);
  CHECK(bus.reg(3) == 6);
  // The register address stays where the last transfer left it.
  CHECK(bus.transfer(request(0x48, 5, {read(2, COSTAR_I2C_MSG_STOP)})) == reply(0x48, 5, 0, {0, 0}));

  // A partly written register keeps its other byte; the host sees one change.
  CHECK(bus.transfer(request(0x48, 6, {write({0x01, 0xff}, COSTAR_I2C_MSG_STOP)})) == reply(0x48, 6, 0));
  CHECK(bus.reg(1) == 0xff34);
  const std::string trace = bus.world.trace().format();
  CHECK(trace.find("m1 set sensor[1]=4660\n") != std::string::npos);   // 0x1234
  CHECK(trace.find("m1 set sensor[1]=65332\n") != std::string::npos);  // 0xff34
  CHECK(trace.find("10000 m1 rx i2c 004801020003011234") != std::string::npos);  // the wire's latency
}

TEST_CASE("i2c-registers options: no auto-increment, little-endian, 16-bit register addresses") {
  Bus bus(R"(
[[model]]
type = "i2c-registers"
bus = "board.i2c"
address = 0x10
registers = "board.sensor"
address_bytes = 2
register_bytes = 3
little_endian = true
auto_increment = false
)");
  bus.world.machine(1).set_register("sensor", 0x0102, 0xaabbcc);
  CHECK(bus.transfer(request(0x10, 0, {write({0x01, 0x02}), read(6)})) ==
        reply(0x10, 0, 0, {0xcc, 0xbb, 0xaa, 0xcc, 0xbb, 0xaa}));
  CHECK(bus.transfer(request(0x10, 1, {write({0x01, 0x02, 0x11, 0x22, 0x33, 0x44}, COSTAR_I2C_MSG_STOP)})) ==
        reply(0x10, 1, 0));
  // Without auto-increment, 0x44 starts the same register again; the stop
  // ends it there.
  CHECK(bus.reg(0x0102) == 0x332244);
  CHECK(bus.reg(0x0103) == 0);
}

TEST_CASE("an i2c-eeprom model writes within a page, reads on through memory, and is busy after a write") {
  Bus bus(kEeprom);
  // 512 bytes with one address byte: the part answers 0x50 and 0x51.
  CHECK(bus.transfer(request(0x52, 0, {read(1)})) == reply(0x52, 0, COSTAR_I2C_NACK_ADDRESS));

  // Three bytes from offset 0x0e of block 1: the page (0x108-0x10f) wraps.
  CHECK(bus.transfer(request(0x51, 1, {write({0x0e, 1, 2, 3}, COSTAR_I2C_MSG_STOP)})) == reply(0x51, 1, 0));
  CHECK(bus.memory()[0x10e] == 1);
  CHECK(bus.memory()[0x10f] == 2);
  CHECK(bus.memory()[0x108] == 3);
  const std::string trace = bus.world.trace().format();
  CHECK(trace.find("m1 mem eeprom+264 03\n") != std::string::npos);
  CHECK(trace.find("m1 mem eeprom+270 0102\n") != std::string::npos);

  // Busy for 2 ms: it NACKs (the controller polls), then answers.
  CHECK(bus.transfer(request(0x51, 2, {write({0x0e}), read(2)})) == reply(0x51, 2, COSTAR_I2C_NACK_ADDRESS));
  bus.world.idle_until(bus.world.now() + milliseconds(2));
  CHECK(bus.transfer(request(0x51, 3, {write({0x0e}), read(2)})) == reply(0x51, 3, 0, {1, 2}));

  // A read runs on through the end of memory back to the start.
  bus.memory()[0] = 0x77;
  CHECK(bus.transfer(request(0x51, 4, {write({0xff}), read(2)})) == reply(0x51, 4, 0, {0xff, 0x77}));
  // A write that only sets the address doesn't make it busy; one without
  // even that (a probe) leaves the address where it was.
  CHECK(bus.transfer(request(0x50, 5, {write({0x00}, COSTAR_I2C_MSG_STOP)})) == reply(0x50, 5, 0));
  CHECK(bus.transfer(request(0x51, 6, {write({}, COSTAR_I2C_MSG_STOP)})) == reply(0x51, 6, 0));
  CHECK(bus.transfer(request(0x50, 7, {read(1, COSTAR_I2C_MSG_STOP)})) == reply(0x50, 7, 0, {0x77}));
}

TEST_CASE("only a whole memory or register address moves an I2C part's pointer") {
  Bus bus(R"(
[[model]]
type = "i2c-eeprom"
bus = "board.i2c"
address = 0x50
memory = "board.eeprom"
address_bytes = 2
page_size = 8

[[model]]
type = "i2c-registers"
bus = "board.i2c"
address = 0x48
registers = "board.sensor"
address_bytes = 2
)");
  bus.memory()[0x000] = 0x11;
  bus.memory()[0x105] = 0x22;
  bus.memory()[0x106] = 0x66;
  bus.memory()[0x1ff] = 0x33;
  // 512 bytes with two address bytes: one of them, then a repeated start,
  // leaves the address where it was (0xff00 is beyond the memory).
  CHECK(bus.transfer(request(0x50, 0, {write({0xff}), read(1)})) == reply(0x50, 0, 0, {0x11}));
  CHECK(bus.transfer(request(0x50, 1, {write({0x01, 0x05}), read(1)})) == reply(0x50, 1, 0, {0x22}));
  CHECK(bus.transfer(request(0x50, 2, {write({0xff}, COSTAR_I2C_MSG_STOP), read(1, COSTAR_I2C_MSG_STOP)})) ==
        reply(0x50, 2, 0, {0x66}));  // on from 0x105
  // A whole address wraps around the memory, as the part ignores the bits
  // beyond its size: 0xffff is 0x1ff.
  CHECK(bus.transfer(request(0x50, 3, {write({0xff, 0xff}), read(2)})) == reply(0x50, 3, 0, {0x33, 0x11}));

  bus.world.machine(1).set_register("sensor", 0x0102, 0x44);
  bus.world.machine(1).set_register("sensor", 0x00ff, 0x55);
  CHECK(bus.transfer(request(0x48, 4, {write({0x01, 0x02}), read(1)})) == reply(0x48, 4, 0, {0x44}));
  CHECK(bus.transfer(request(0x48, 5, {write({0xff}), read(1)})) == reply(0x48, 5, 0, {0x00}));  // 0x0103
}

TEST_CASE("several models share a bus; a fault window makes one NACK") {
  Bus bus(kSensor + kEeprom, R"(
[[model]]
type = "i2c-registers"
bus = "board.i2c"
address = 0x49
registers = "board.sensor"
faults = [{ kind = "nack", from_ms = 1, until_ms = 2 }]
)");
  bus.world.machine(1).set_register("sensor", 0, 0x0102);
  CHECK(bus.transfer(request(0x49, 0, {write({0x00}), read(1)})) == reply(0x49, 0, 0, {0x02}));
  CHECK(bus.transfer(request(0x48, 0, {write({0x00}), read(2)})) == reply(0x48, 0, 0, {0x01, 0x02}));
  bus.world.idle_until(milliseconds(1));
  CHECK(bus.transfer(request(0x49, 1, {write({0x00}), read(1)})) == reply(0x49, 1, COSTAR_I2C_NACK_ADDRESS));
  CHECK(bus.transfer(request(0x48, 1, {write({0x00}), read(2)})) == reply(0x48, 1, 0, {0x01, 0x02}));
  bus.world.idle_until(milliseconds(2));
  CHECK(bus.transfer(request(0x49, 2, {write({0x00}), read(1)})) == reply(0x49, 2, 0, {0x02}));
}

TEST_CASE("a powered-off board answers nothing") {
  Bus bus(kSensor);
  bus.world.machine(1).power_off();
  bus.world.machine(0).send("i2c", request(0x48, 0, {read(1)}));
  bus.world.run();
  CHECK_FALSE(bus.world.machine(0).receive("i2c").has_value());
}

TEST_CASE("scenario models are checked when the scenario loads") {
  const auto rejects = [](const std::string& models, const std::string& message) {
    CHECK_THROWS_WITH(costar::parse_scenario(kBoard + models, "test.toml"), doctest::Contains(message));
  };
  rejects("[[model]]\ntype = \"i2c-mux\"\nbus = \"board.i2c\"", "model 1: type must be i2c-registers or i2c-eeprom");
  rejects("[[model]]\ntype = \"i2c-registers\"\nbus = \"board.sensor\"", "board.sensor is not a channel device");
  rejects("[[model]]\ntype = \"i2c-registers\"\nbus = \"board.i2c\"\nregisters = \"board.sensor\"",
          "model 1 needs its 7-bit I2C address");
  rejects("[[model]]\ntype = \"i2c-registers\"\nbus = \"board.i2c\"\naddress = 128\nregisters = \"board.sensor\"",
          "address must be between 0 and 127");
  rejects("[[model]]\ntype = \"i2c-registers\"\nbus = \"board.i2c\"\naddress = 1\nregisters = \"board.eeprom\"",
          "board.eeprom is not a registers device");
  rejects("[[model]]\ntype = \"i2c-registers\"\nbus = \"board.i2c\"\naddress = 1\nregisters = \"board.sensor\"\n"
          "register_bytes = 5",
          "register_bytes must be between 1 and 4");
  rejects("[[model]]\ntype = \"i2c-registers\"\nbus = \"board.i2c\"\naddress = 1\nregisters = \"board.sensor\"\n"
          "page_size = 8",
          "model 1: unknown key 'page_size'");
  rejects("[[model]]\ntype = \"i2c-eeprom\"\nbus = \"board.i2c\"\naddress = 0x50\nmemory = \"board.eeprom\"",
          "model 1 needs its page_size");
  rejects("[[model]]\ntype = \"i2c-eeprom\"\nbus = \"board.i2c\"\naddress = 0x7f\nmemory = \"board.eeprom\"\n"
          "page_size = 8",
          "needs more I2C addresses");
  rejects(kEeprom + "[[model]]\ntype = \"i2c-registers\"\nbus = \"board.i2c\"\naddress = 0x51\n"
                    "registers = \"board.sensor\"",
          "model 2: another model on board.i2c answers I2C address 81");
  rejects(kSensor + "faults = [{ kind = \"drop\" }]", "model 1, fault: kind must be nack");
  // A model answers for parts the firmware talks to, not for firmware.
  const std::string firmware = "[[machine]]\nname = \"node\"\nimage = \"freertos_link_node.so\"\n"
                               "devices = [{ name = \"i2c\", kind = \"channel\" }]\n";
  CHECK_THROWS_WITH(costar::parse_scenario(kBoard + firmware + "[[model]]\ntype = \"i2c-registers\"\nbus = \"node.i2c\"",
                                           "test.toml", {std::filesystem::path(LINK_NODE).parent_path()}),
                    doctest::Contains("model 1: its bus node.i2c must be a device of a passive machine"));
}
