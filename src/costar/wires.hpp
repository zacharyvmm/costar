#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include "costar/devices.hpp"
#include "costar/time.hpp"

namespace costar {

// A channel device on a machine.
struct Endpoint {
  uint32_t machine;
  std::string device;

  bool operator==(const Endpoint&) const = default;
};

// Something that happens to frames sent during [from, until).
struct WireFault {
  // Corrupt inverts the first byte; Delay adds `delay` to the latency, so a
  // delayed frame can arrive after later ones.
  enum class Kind { Drop, Corrupt, Delay };
  Kind kind;
  Time from = 0;
  Time until = kNever;
  Time delay = 0;
  // Only frames that start with these bytes (a CAN ID, say); empty: all.
  Frame match = {};

  bool applies(const Frame& frame, Time sent) const;
};

// A frame on its way to an endpoint.
struct Delivery {
  Time at;
  Endpoint to;
  Frame frame;
};

// Connects channel devices.  A frame sent by one endpoint reaches every other
// endpoint: a two-endpoint wire is a point-to-point link, a larger one a bus.
struct Wire {
  std::vector<Endpoint> endpoints;
  Time latency = 0;
  // Transmission speed; 0 means frames take no time on the wire.
  uint64_t bits_per_second = 0;
  // Bits on the wire per byte (10 for a UART: start, 8 data, stop).
  uint32_t bits_per_byte = 8;
  // Deliver frames byte by byte, as a UART does.
  bool byte_stream = false;
  // A shared medium (a bus such as CAN) carries one frame at a time; otherwise
  // only each sender's own frames are serialised.
  bool shared_medium = false;
  std::vector<WireFault> faults = {};

  // Transmission state: when the medium, or each sender, is next free.
  Time medium_busy_until = 0;
  std::vector<std::pair<Endpoint, Time>> sender_busy_until = {};

  bool attaches(const Endpoint& endpoint) const;
  // Send `frame` from `from` at `now`.  Returns the resulting deliveries, or
  // nothing if a fault dropped the frame.
  std::optional<std::vector<Delivery>> transmit(const Endpoint& from, Frame frame, Time now);

 private:
  Time& busy_until(const Endpoint& sender);
  Time transmission_time(size_t bytes) const;
};

}  // namespace costar
