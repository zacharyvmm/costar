#include "costar/wires.hpp"

#include <algorithm>
#include <ranges>
#include <utility>

namespace costar {

namespace {

// An unsigned 128-bit number as two 64-bit halves: 32-bit hosts have no
// 128-bit integers.
struct Wide {
  uint64_t hi, lo;
};

// a * b, exactly, from 32-bit halves.
Wide multiply(uint64_t a, uint64_t b) {
  const uint64_t a_lo = a & 0xffffffff, a_hi = a >> 32, b_lo = b & 0xffffffff, b_hi = b >> 32;
  const uint64_t low = a_lo * b_lo, cross1 = a_lo * b_hi, cross2 = a_hi * b_lo;
  const uint64_t mid = (low >> 32) + (cross1 & 0xffffffff) + (cross2 & 0xffffffff);
  return {.hi = a_hi * b_hi + (cross1 >> 32) + (cross2 >> 32) + (mid >> 32), .lo = (mid << 32) | (low & 0xffffffff)};
}

// n / d and n % d, for d > 0 and a quotient that fits in 64 bits (n.hi < d):
// long division, bit by bit, with the remainder left in n.hi.
std::pair<uint64_t, uint64_t> divide(Wide n, uint64_t d) {
  uint64_t quotient = 0;
  for (int bit = 0; bit < 64; ++bit) {
    const bool carry = n.hi >> 63;
    n.hi = n.hi << 1 | n.lo >> 63;
    n.lo <<= 1;
    quotient <<= 1;
    if (carry || n.hi >= d) {
      n.hi -= d;
      quotient |= 1;
    }
  }
  return {quotient, n.hi};
}

}  // namespace

bool Wire::attaches(const Endpoint& endpoint) const {
  return std::ranges::find(endpoints, endpoint) != endpoints.end();
}

bool WireFault::applies(const Frame& frame, Time sent) const {
  if (sent < from || sent >= until || frame.size() < match.size()) return false;
  return std::ranges::equal(match, frame | std::views::take(match.size()));
}

std::optional<std::vector<Delivery>> Wire::transmit(const Endpoint& from, Frame frame, Time now) {
  Time delay = 0;
  if (!faults.empty()) {
    const Frame sent = frame;  // faults match the frame as it was sent
    for (const WireFault& fault : faults) {
      if (!fault.applies(sent, now)) continue;
      switch (fault.kind) {
        case WireFault::Kind::Drop: return std::nullopt;
        case WireFault::Kind::Corrupt:
          if (!frame.empty()) frame[0] = ~frame[0];
          break;
        case WireFault::Kind::Delay: delay = time_add(delay, fault.delay); break;
      }
    }
  }

  std::vector<Frame> pieces;
  if (byte_stream) {
    for (uint8_t byte : frame) pieces.push_back({byte});
  } else {
    pieces.push_back(std::move(frame));
  }

  // Each piece occupies the transmitter (or the whole medium) while it is sent.
  std::vector<Delivery> deliveries;
  Time& busy = busy_until(from);
  for (const Frame& piece : pieces) {
    const Time start = std::max(now, busy);
    busy = time_add(start, transmission_time(piece.size()));
    for (const Endpoint& to : endpoints) {
      if (to != from) deliveries.push_back({time_add(time_add(busy, latency), delay), to, piece});
    }
  }
  return deliveries;
}

Time Wire::transmission_time(uint64_t bytes) const {
  if (bits_per_second == 0) return 0;
  // ceil(bits * 10^9 / bits_per_second) ns, where bits takes up to 96 bits.
  // In whole seconds and a remainder, bits = seconds * bits_per_second + rest,
  // that is seconds * 10^9 + ceil(rest * 10^9 / bits_per_second), with
  // rest < bits_per_second.
  const Wide bits = multiply(bytes, bits_per_byte);
  if (bits.hi >= bits_per_second) return kNever;  // 2^64 s or more
  const auto [seconds, rest] = divide(bits, bits_per_second);
  const auto [ns, ns_rest] = divide(multiply(rest, 1'000'000'000), bits_per_second);  // ns < 10^9
  uint64_t whole;
  if (__builtin_mul_overflow(seconds, uint64_t{1'000'000'000}, &whole)) return kNever;
  return time_add(whole, ns + (ns_rest != 0));
}

Time& Wire::busy_until(const Endpoint& sender) {
  if (shared_medium) return medium_busy_until;
  for (auto& [endpoint, until] : sender_busy_until) {
    if (endpoint == sender) return until;
  }
  return sender_busy_until.emplace_back(sender, 0).second;
}

}  // namespace costar
