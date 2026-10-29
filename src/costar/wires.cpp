#include "costar/wires.hpp"

#include <algorithm>

namespace costar {

bool Wire::attaches(const Endpoint& endpoint) const {
  return std::ranges::find(endpoints, endpoint) != endpoints.end();
}

std::optional<std::vector<Delivery>> Wire::transmit(const Endpoint& from, Frame frame, Time now) {
  for (const WireFault& fault : faults) {
    if (now < fault.from || now >= fault.until) continue;
    if (fault.kind == WireFault::Kind::Drop) return std::nullopt;
    if (fault.kind == WireFault::Kind::Corrupt && !frame.empty()) frame[0] = ~frame[0];
  }

  // The frame occupies the transmitter (or the whole medium) while it is sent.
  Time& busy = busy_until(from);
  const Time start = std::max(now, busy);
  const Time duration =
      bits_per_second == 0 ? 0 : (frame.size() * 8 * 1'000'000'000 + bits_per_second - 1) / bits_per_second;
  busy = start + duration;

  std::vector<Delivery> deliveries;
  for (const Endpoint& to : endpoints) {
    if (to != from) deliveries.push_back({start + duration + latency, to, frame});
  }
  return deliveries;
}

Time& Wire::busy_until(const Endpoint& sender) {
  if (shared_medium) return medium_busy_until;
  for (auto& [endpoint, until] : sender_busy_until) {
    if (endpoint == sender) return until;
  }
  return sender_busy_until.emplace_back(sender, 0).second;
}

}  // namespace costar
