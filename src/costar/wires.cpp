#include "costar/wires.hpp"

#include <algorithm>
#include <ranges>

namespace costar {

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

Time Wire::transmission_time(size_t bytes) const {
  if (bits_per_second == 0) return 0;
  const unsigned __int128 bits = static_cast<unsigned __int128>(bytes) * bits_per_byte;
  const unsigned __int128 ns = (bits * 1'000'000'000 + bits_per_second - 1) / bits_per_second;
  return ns >= kNever ? kNever : static_cast<Time>(ns);
}

Time& Wire::busy_until(const Endpoint& sender) {
  if (shared_medium) return medium_busy_until;
  for (auto& [endpoint, until] : sender_busy_until) {
    if (endpoint == sender) return until;
  }
  return sender_busy_until.emplace_back(sender, 0).second;
}

}  // namespace costar
