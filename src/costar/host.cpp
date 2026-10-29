#include "costar/host.hpp"

#include <arpa/inet.h>
#include <poll.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

#include <cerrno>
#include <charconv>
#include <cstring>
#include <stdexcept>
#include <system_error>
#include <utility>

#ifdef __linux__
#include <linux/if.h>
#include <linux/if_tun.h>
#include <sys/ioctl.h>
#endif

namespace costar {

namespace {

[[noreturn]] void fail(const std::string& what) { throw std::system_error(errno, std::generic_category(), what); }

// An owned file descriptor.
class Fd {
 public:
  explicit Fd(int fd = -1) : fd_(fd) {}
  ~Fd() { reset(); }
  Fd(Fd&& other) noexcept : fd_(std::exchange(other.fd_, -1)) {}
  Fd& operator=(Fd&& other) noexcept {
    reset(std::exchange(other.fd_, -1));
    return *this;
  }
  int get() const { return fd_; }
  explicit operator bool() const { return fd_ >= 0; }
  void reset(int fd = -1) {
    if (fd_ >= 0) ::close(fd_);
    fd_ = fd;
  }

 private:
  int fd_;
};

// "a.b.c.d:port" (IPv4), with a port of 0..65535.
sockaddr_in parse_address(const std::string& text) {
  const size_t colon = text.rfind(':');
  sockaddr_in address{};
  address.sin_family = AF_INET;
  unsigned port = 0;
  const char* digits = colon == std::string::npos ? nullptr : text.data() + colon + 1;
  const char* end = text.data() + text.size();
  if (digits == nullptr || inet_pton(AF_INET, text.substr(0, colon).c_str(), &address.sin_addr) != 1 ||
      digits == end || std::from_chars(digits, end, port).ptr != end || port > 65535) {
    throw std::invalid_argument("host address should be a.b.c.d:port (port 0..65535), not '" + text + "'");
  }
  address.sin_port = htons(static_cast<uint16_t>(port));
  return address;
}

std::string format_address(const sockaddr_in& address) {
  char host[INET_ADDRSTRLEN];
  inet_ntop(AF_INET, &address.sin_addr, host, sizeof(host));
  return std::string(host) + ":" + std::to_string(ntohs(address.sin_port));
}

std::string bound_address(int fd) {
  sockaddr_in address{};
  socklen_t length = sizeof(address);
  if (getsockname(fd, reinterpret_cast<sockaddr*>(&address), &length) != 0) fail("getsockname");
  return format_address(address);
}

Fd open_socket(int type) {
  Fd fd(::socket(AF_INET, type | SOCK_CLOEXEC | SOCK_NONBLOCK, 0));
  if (!fd) fail("socket");
  return fd;
}

// UDP binds exclusively (address reuse would let another program take the
// traffic); TCP listeners reuse addresses so a restart need not wait out
// TIME_WAIT.
void bind_to(int fd, const std::string& local, bool reuse_address) {
  const sockaddr_in address = parse_address(local);
  if (reuse_address) {
    const int on = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &on, sizeof(on));
  }
  if (::bind(fd, reinterpret_cast<const sockaddr*>(&address), sizeof(address)) != 0) fail("bind " + local);
}

constexpr size_t kMaxFrame = 65536;

// ── UDP ───────────────────────────────────────────────────────────────

class UdpLink final : public HostLink {
 public:
  UdpLink(const std::string& local, const std::string& remote) : socket_(open_socket(SOCK_DGRAM)) {
    bind_to(socket_.get(), local, false);
    if (!remote.empty()) {
      remote_ = parse_address(remote);
      fixed_remote_ = has_remote_ = true;
    }
  }

  void send(const Frame& frame) override {
    if (!has_remote_) return;  // nobody to send to yet
    ::sendto(socket_.get(), frame.data(), frame.size(), MSG_NOSIGNAL, reinterpret_cast<const sockaddr*>(&remote_),
             sizeof(remote_));
  }

  std::vector<Frame> receive() override {
    std::vector<Frame> frames;
    for (;;) {
      sockaddr_in sender{};
      socklen_t length = sizeof(sender);
      const ssize_t n = ::recvfrom(socket_.get(), buffer_.data(), buffer_.size(), 0,
                                   reinterpret_cast<sockaddr*>(&sender), &length);
      if (n < 0 && errno == EINTR) continue;
      if (n < 0) break;
      frames.emplace_back(buffer_.begin(), buffer_.begin() + n);
      if (!fixed_remote_) remote_ = sender, has_remote_ = true;
    }
    return frames;
  }

  std::string address() const override { return bound_address(socket_.get()); }

 private:
  Fd socket_;
  Frame buffer_ = Frame(kMaxFrame);
  sockaddr_in remote_{};
  bool has_remote_ = false;
  bool fixed_remote_ = false;
};

// ── TCP ───────────────────────────────────────────────────────────────

// Frames on a stream, each preceded by its length (2 bytes, big endian).
// Every socket is non-blocking: output waits in a queue (kept across
// partial writes) so a peer that stops reading never stalls the simulation.
class TcpLink final : public HostLink {
 public:
  static std::unique_ptr<TcpLink> listening(const std::string& address) {
    auto link = std::unique_ptr<TcpLink>(new TcpLink(address));
    link->listener_ = open_socket(SOCK_STREAM);
    bind_to(link->listener_.get(), address, true);
    if (::listen(link->listener_.get(), 1) != 0) fail("listen " + address);
    return link;
  }

  // Connects in the background; a failure surfaces on a later poll.
  static std::unique_ptr<TcpLink> connecting(const std::string& address) {
    auto link = std::unique_ptr<TcpLink>(new TcpLink(address));
    link->peer_ = open_socket(SOCK_STREAM);
    const sockaddr_in remote = parse_address(address);
    if (::connect(link->peer_.get(), reinterpret_cast<const sockaddr*>(&remote), sizeof(remote)) == 0) {
      link->connected_ = true;
    } else if (errno != EINPROGRESS) {
      fail("connect " + address);
    }
    return link;
  }

  void send(const Frame& frame) override {
    if (frame.size() > 0xFFFF) throw std::length_error("TCP link frames are at most 65535 bytes");
    update_peer();
    if (!peer_) return;  // no client yet: nobody to send to
    if (output_.size() + frame.size() + 2 > kMaxQueued) {
      if (listener_) {
        drop_peer();  // a client that stopped reading; wait for the next
        return;
      }
      throw std::runtime_error("TCP link to " + address_ + ": the peer is not reading");
    }
    output_.push_back(static_cast<uint8_t>(frame.size() >> 8));
    output_.push_back(static_cast<uint8_t>(frame.size()));
    output_.insert(output_.end(), frame.begin(), frame.end());
    flush();
  }

  std::vector<Frame> receive() override {
    update_peer();
    if (!peer_) return {};
    flush();
    while (peer_ && !read_closed_) {
      const ssize_t n = ::recv(peer_.get(), buffer_.data(), buffer_.size(), 0);
      if (n > 0) {
        input_.insert(input_.end(), buffer_.begin(), buffer_.begin() + n);
      } else if (n == 0) {
        read_closed_ = true;  // the peer finished sending; it may still read
      } else if (errno == EINTR) {
        continue;
      } else if (errno == EAGAIN || errno == EWOULDBLOCK) {
        break;
      } else {
        lost("receive failed");
      }
    }
    return decode();
  }

  std::string address() const override { return listener_ ? bound_address(listener_.get()) : std::string(); }

 private:
  static constexpr size_t kMaxQueued = 16 << 20;

  explicit TcpLink(std::string address) : address_(std::move(address)) {}

  // Finish connecting, or accept a client (replacing one that has finished
  // sending).
  void update_peer() {
    if (!listener_ && !connected_) {
      pollfd p{peer_.get(), POLLOUT, 0};
      if (::poll(&p, 1, 0) != 1) return;
      int error = 0;
      socklen_t length = sizeof(error);
      getsockopt(peer_.get(), SOL_SOCKET, SO_ERROR, &error, &length);
      if (error != 0) {
        errno = error;
        fail("connect " + address_);
      }
      connected_ = true;
    }
    if (listener_ && (!peer_ || read_closed_)) {
      Fd client(::accept4(listener_.get(), nullptr, nullptr, SOCK_CLOEXEC | SOCK_NONBLOCK));
      if (client) {
        drop_peer();
        peer_ = std::move(client);
      }
    }
  }

  // Write as much queued output as the socket takes now.
  void flush() {
    if (!connected_ && !listener_) return;
    size_t sent = 0;
    while (sent < output_.size()) {
      const ssize_t n = ::send(peer_.get(), output_.data() + sent, output_.size() - sent, MSG_NOSIGNAL);
      if (n > 0) {
        sent += static_cast<size_t>(n);
      } else if (n < 0 && errno == EINTR) {
        continue;
      } else if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
        break;
      } else {
        lost("send failed");
        return;
      }
    }
    output_.erase(output_.begin(), output_.begin() + sent);
  }

  std::vector<Frame> decode() {
    std::vector<Frame> frames;
    size_t offset = 0;
    while (input_.size() - offset >= 2) {
      const size_t length = static_cast<size_t>(input_[offset]) << 8 | input_[offset + 1];
      if (input_.size() - offset - 2 < length) break;
      frames.emplace_back(input_.begin() + offset + 2, input_.begin() + offset + 2 + length);
      offset += 2 + length;
    }
    input_.erase(input_.begin(), input_.begin() + offset);
    return frames;
  }

  // The connection broke.  A listener waits for its next client; an outbound
  // link has nowhere to go, so the run stops with an error.
  void lost(const char* what) {
    if (!listener_) {
      const int error = errno;
      drop_peer();
      errno = error;
      fail("TCP link to " + address_ + ": " + what);
    }
    drop_peer();
  }

  void drop_peer() {
    peer_.reset();
    read_closed_ = false;
    input_.clear();
    output_.clear();
  }

  std::string address_;
  Fd listener_;
  Fd peer_;
  bool connected_ = false;  // outbound links only
  bool read_closed_ = false;
  Frame buffer_ = Frame(kMaxFrame);
  Frame input_;
  Frame output_;
};

// ── TAP ───────────────────────────────────────────────────────────────

#ifdef __linux__
class TapLink final : public HostLink {
 public:
  explicit TapLink(const std::string& interface) : tap_(::open("/dev/net/tun", O_RDWR | O_NONBLOCK | O_CLOEXEC)) {
    if (!tap_) fail("open /dev/net/tun");
    ifreq request{};
    request.ifr_flags = IFF_TAP | IFF_NO_PI;
    std::strncpy(request.ifr_name, interface.c_str(), IFNAMSIZ - 1);
    if (::ioctl(tap_.get(), TUNSETIFF, &request) != 0) fail("attach to TAP interface " + interface);
  }

  void send(const Frame& frame) override { (void)!::write(tap_.get(), frame.data(), frame.size()); }

  std::vector<Frame> receive() override {
    std::vector<Frame> frames;
    for (;;) {
      const ssize_t n = ::read(tap_.get(), buffer_.data(), buffer_.size());
      if (n < 0 && errno == EINTR) continue;
      if (n <= 0) break;
      frames.emplace_back(buffer_.begin(), buffer_.begin() + n);
    }
    return frames;
  }

 private:
  Fd tap_;
  Frame buffer_ = Frame(kMaxFrame);
};
#endif

}  // namespace

std::unique_ptr<HostLink> udp_link(const std::string& local, const std::string& remote) {
  return std::make_unique<UdpLink>(local, remote);
}

std::unique_ptr<HostLink> tcp_listen_link(const std::string& address) { return TcpLink::listening(address); }

std::unique_ptr<HostLink> tcp_connect_link(const std::string& address) { return TcpLink::connecting(address); }

std::unique_ptr<HostLink> tap_link(const std::string& interface) {
#ifdef __linux__
  return std::make_unique<TapLink>(interface);
#else
  (void)interface;
  throw std::runtime_error("TAP links need Linux");
#endif
}

}  // namespace costar
