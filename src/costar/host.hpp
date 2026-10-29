#pragma once

#include <memory>
#include <string>
#include <vector>

#include "costar/devices.hpp"

namespace costar {

// The far end of a channel, outside the simulation: a host socket or a TAP
// interface.  Host links are not deterministic: frames from the host arrive
// at whatever virtual time the world polls them (see World::run_realtime).
class HostLink {
 public:
  virtual ~HostLink() = default;
  // A frame the firmware sent; goes out at once.
  virtual void send(const Frame& frame) = 0;
  // Frames that arrived from the host since the last call.  Never blocks.
  virtual std::vector<Frame> receive() = 0;
  // Where the host can reach this link ("host:port"), if it listens.
  virtual std::string address() const { return {}; }
};

// Each UDP datagram is one frame.  Bound to `local` ("host:port", exclusively);
// sends to `remote`, or, if that is empty, to whoever sent the last datagram.
std::unique_ptr<HostLink> udp_link(const std::string& local, const std::string& remote);

// Frames on a TCP stream, each preceded by its length (2 bytes, big endian).
// Never blocks: output to a slow peer is queued.
// Listens on `address` and serves one client at a time; a client that has
// finished sending is replaced by the next one to connect.
std::unique_ptr<HostLink> tcp_listen_link(const std::string& address);
// Connects to `address` in the background.  If the connection fails or
// breaks, the next send or receive throws.
std::unique_ptr<HostLink> tcp_connect_link(const std::string& address);

// Ethernet frames on a Linux TAP interface.  Needs CAP_NET_ADMIN, or a
// persistent TAP the user owns (`ip tuntap add mode tap user $USER name tap0`).
std::unique_ptr<HostLink> tap_link(const std::string& interface);

}  // namespace costar
