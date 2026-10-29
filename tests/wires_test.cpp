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
  Wire slow{.endpoints = {a, b}, .latency = 20};
  CHECK(slow.transmit(a, Frame{1}, costar::kNever - 10)->front().at == costar::kNever);
}
