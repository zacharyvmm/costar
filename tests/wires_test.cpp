#include <doctest/doctest.h>

#include "costar/wires.hpp"

using costar::Delivery;
using costar::Endpoint;
using costar::Frame;
using costar::milliseconds;
using costar::Time;
using costar::Wire;
using costar::WireFault;

namespace {

constexpr Time kMicrosecond = 1'000;

const Endpoint a{0, "can"};
const Endpoint b{1, "can"};
const Endpoint c{2, "can"};

std::vector<Time> arrival_times(const std::vector<Delivery>& deliveries) {
  std::vector<Time> out;
  for (const Delivery& d : deliveries) out.push_back(d.at);
  return out;
}

}  // namespace

TEST_CASE("a link delivers to the other end after its latency") {
  Wire wire{.endpoints = {a, b}, .latency = milliseconds(5)};
  const auto deliveries = wire.transmit(a, Frame{1, 2}, milliseconds(10));
  REQUIRE(deliveries.has_value());
  REQUIRE(deliveries->size() == 1);
  CHECK(deliveries->front().to == b);
  CHECK(deliveries->front().at == milliseconds(15));
  CHECK(deliveries->front().frame == Frame{1, 2});
}

TEST_CASE("frames take time to transmit, one after another per sender") {
  Wire wire{.endpoints = {a, b}, .bits_per_second = 1'000'000};  // 2 bytes = 16 µs
  CHECK(arrival_times(*wire.transmit(a, Frame{1, 2}, 0)) == std::vector<Time>{16 * kMicrosecond});
  CHECK(arrival_times(*wire.transmit(a, Frame{3, 4}, 0)) == std::vector<Time>{32 * kMicrosecond});
  // A different sender on a full-duplex link is not held up.
  CHECK(arrival_times(*wire.transmit(b, Frame{5, 6}, 0)) == std::vector<Time>{16 * kMicrosecond});
}

TEST_CASE("a bus reaches every other node and carries one frame at a time") {
  Wire bus{.endpoints = {a, b, c}, .bits_per_second = 1'000'000, .shared_medium = true};
  const auto first = bus.transmit(a, Frame{1, 2}, 0);
  REQUIRE(first->size() == 2);
  CHECK(first->at(0).to == b);
  CHECK(first->at(1).to == c);
  CHECK(arrival_times(*first) == std::vector<Time>{16 * kMicrosecond, 16 * kMicrosecond});
  // b sends at the same instant but must wait for the medium.
  CHECK(arrival_times(*bus.transmit(b, Frame{3, 4}, 0)) == std::vector<Time>{32 * kMicrosecond, 32 * kMicrosecond});
}

TEST_CASE("faults apply to frames sent inside their window") {
  Wire wire{.endpoints = {a, b},
            .faults = {{WireFault::Kind::Drop, milliseconds(5), milliseconds(10)},
                       {WireFault::Kind::Corrupt, milliseconds(20), milliseconds(30)}}};
  CHECK(wire.transmit(a, Frame{0x0f}, milliseconds(4))->front().frame == Frame{0x0f});
  CHECK_FALSE(wire.transmit(a, Frame{0x0f}, milliseconds(5)).has_value());
  CHECK(wire.transmit(a, Frame{0x0f}, milliseconds(10))->front().frame == Frame{0x0f});
  CHECK(wire.transmit(a, Frame{0x0f, 0x01}, milliseconds(25))->front().frame == Frame{0xf0, 0x01});
}

TEST_CASE("wire timing saturates instead of wrapping") {
  Wire fast{.endpoints = {a, b}, .bits_per_second = UINT64_MAX};
  CHECK(fast.transmit(a, Frame{1}, 0)->front().at == 1);  // 8 bits at ~1.8e19 bit/s rounds up to 1 ns
  // 4e10 bits is 4e19 bit·ns, more than 64 bits hold: 4e19 / 3e9 = 13333333333.3 ns.
  Wire wide{.endpoints = {a, b}, .bits_per_second = 3'000'000'000, .bits_per_byte = 4'000'000'000};
  CHECK(wide.transmit(a, Frame(10), 0)->front().at == 13'333'333'334);
  Wire glacial{.endpoints = {a, b}, .bits_per_second = 1, .bits_per_byte = UINT32_MAX};
  CHECK(glacial.transmit(a, Frame(5), 0)->front().at == costar::kNever);
  Wire slow{.endpoints = {a, b}, .latency = 20};
  CHECK(slow.transmit(a, Frame{1}, costar::kNever - 10)->front().at == costar::kNever);
}

TEST_CASE("wire timing is exact when bits * 10^9 needs more than 64 bits") {
  constexpr uint64_t kMax = UINT64_MAX;
  // 2^33 bytes of 2^31 bits is 2^64 bits, more than 64 bits hold, yet at
  // 2^64 - 1 bit/s they take just over a second: 10^9 + 10^9 / (2^64 - 1) ns.
  Wire fastest{.bits_per_second = kMax, .bits_per_byte = 1u << 31};
  CHECK(fastest.transmission_time(uint64_t{1} << 33) == 1'000'000'001);
  CHECK(fastest.transmission_time(uint64_t{1} << 34) == 2'000'000'001);
  // The most bits there can be, (2^64 - 1)(2^32 - 1), at 2^64 - 1 bit/s.
  Wire widest{.bits_per_second = kMax, .bits_per_byte = UINT32_MAX};
  CHECK(widest.transmission_time(kMax) == 4'294'967'295'000'000'000);
  // At 1 bit/ns: kNever - 1 ns, the longest representable time, then kNever.
  Wire gigabit{.bits_per_second = 1'000'000'000, .bits_per_byte = 1};
  CHECK(gigabit.transmission_time(kMax - 1) == costar::kNever - 1);
  CHECK(gigabit.transmission_time(kMax) == costar::kNever);
  // 2^64 bits at 10^9 bit/s: 2^64 ns.
  Wire wide{.bits_per_second = 1'000'000'000, .bits_per_byte = 1u << 31};
  CHECK(wide.transmission_time(uint64_t{1} << 33) == costar::kNever);
  // 2^64 seconds and more.
  Wire slowest{.bits_per_second = 1, .bits_per_byte = UINT32_MAX};
  CHECK(slowest.transmission_time(kMax) == costar::kNever);
  CHECK(Wire{}.transmission_time(kMax) == 0);  // no speed: no time
}

TEST_CASE("a delay fault holds back matching frames, which later ones overtake") {
  Wire wire{.endpoints = {a, b},
            .latency = milliseconds(1),
            .faults = {{.kind = WireFault::Kind::Delay, .from = milliseconds(5), .delay = milliseconds(10),
                        .match = {0x01, 0x23}}}};
  CHECK(wire.transmit(a, Frame{0x01, 0x23, 0xff}, milliseconds(4))->front().at == milliseconds(5));  // before the window
  CHECK(wire.transmit(a, Frame{0x01, 0x23, 0xff}, milliseconds(5))->front().at == milliseconds(16));
  CHECK(wire.transmit(a, Frame{0x01, 0x24, 0xff}, milliseconds(6))->front().at == milliseconds(7));  // no match
  CHECK(wire.transmit(a, Frame{0x01}, milliseconds(6))->front().at == milliseconds(7));  // too short to match
}

TEST_CASE("faults match the frame as sent, before another fault corrupts it") {
  Wire wire{.endpoints = {a, b},
            .faults = {{.kind = WireFault::Kind::Corrupt}, {.kind = WireFault::Kind::Drop, .match = {0x10}}}};
  CHECK_FALSE(wire.transmit(a, Frame{0x10, 0x00}, 0).has_value());
  CHECK(wire.transmit(a, Frame{0xef, 0x00}, 0)->front().frame == Frame{0x10, 0x00});
}
