#pragma once

#include <cstdint>
#include <functional>
#include <memory>
#include <optional>
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

// How a stream (TCP) carries frames.
enum class Framing {
  Length,  // each frame preceded by its length (2 bytes, big endian)
  Raw,     // a byte stream, as a UART is: the host's bytes become frames as
           // they arrive (at most 64 KiB each), and frames go out as they are
};

// Frames on a TCP stream.  Never blocks: output to a slow peer is queued.
// Listens on `address` and serves one client at a time; a client that has
// finished sending is replaced by the next one to connect.
std::unique_ptr<HostLink> tcp_listen_link(const std::string& address, Framing framing = Framing::Length);
// Connects to `address` in the background.  If the connection fails or
// breaks, the next send or receive throws.
std::unique_ptr<HostLink> tcp_connect_link(const std::string& address, Framing framing = Framing::Length);

// A pseudo-terminal in raw mode, as a byte stream (Framing::Raw), for
// terminal programs (screen, picocom, minicom).  address() is its path
// (/dev/pts/N).  With a `link`, a symbolic link there names it too: an
// existing symbolic link is replaced, anything else is an error, and the
// link is removed with the pty.  Never blocks: output nobody reads is queued
// (up to 1 MiB, then dropped), and a program can attach, leave and attach
// again.
std::unique_ptr<HostLink> pty_link(const std::string& link = "");

// Ethernet frames on a Linux TAP interface.  Needs CAP_NET_ADMIN, or a
// persistent TAP the user owns (`ip tuntap add mode tap user $USER name tap0`).
std::unique_ptr<HostLink> tap_link(const std::string& interface);

// A byte stream on two file descriptors the caller keeps open, such as
// standard input and output (`costar run --console`).  Bytes read from `in`
// become frames as they arrive; frames are written to `out` as they are, and
// a write waits for `out` (a terminal) to take them, unless `stopping` says
// the run is ending (it is checked at least every kStopCheckMs while a write
// waits).  An `escape` byte ends the input, and isn't delivered.  Once `out`
// can't be written (its reader has gone: EPIPE, with SIGPIPE ignored), output
// is dropped.  A terminal `out` is opened again, non-blocking, so a write to
// it never blocks; the constructor throws if that can't be done.
class StreamLink final : public HostLink {
 public:
  static constexpr int kStopCheckMs = 100;

  StreamLink(int in, int out, std::optional<uint8_t> escape = std::nullopt, std::function<bool()> stopping = {});
  ~StreamLink() override;
  StreamLink(const StreamLink&) = delete;
  StreamLink& operator=(const StreamLink&) = delete;
  void send(const Frame& frame) override;
  std::vector<Frame> receive() override;
  // The input ended: at its end of file (or an error), or at the escape byte.
  bool ended() const { return ended_; }
  bool escaped() const { return escaped_; }
  // `out` can't be written any more.
  bool closed() const { return closed_; }
  // Bytes written to `out` so far.
  uint64_t written() const { return written_; }

 private:
  int in_;
  int out_;
  int own_out_ = -1;  // a terminal `out`, opened again non-blocking
  std::optional<uint8_t> escape_;
  std::function<bool()> stopping_;
  bool ended_ = false;
  bool escaped_ = false;
  bool closed_ = false;
  uint64_t written_ = 0;
};

}  // namespace costar
