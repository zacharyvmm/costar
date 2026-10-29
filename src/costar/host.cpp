#include "costar/host.hpp"

#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

#include <cerrno>
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

// "a.b.c.d:port" (IPv4).
sockaddr_in parse_address(const std::string& text) {
  const size_t colon = text.rfind(':');
  sockaddr_in address{};
  address.sin_family = AF_INET;
  if (colon == std::string::npos ||
      inet_pton(AF_INET, text.substr(0, colon).c_str(), &address.sin_addr) != 1) {
    throw std::invalid_argument("host address should be a.b.c.d:port, not '" + text + "'");
  }
  address.sin_port = htons(static_cast<uint16_t>(std::stoul(text.substr(colon + 1))));
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
  Fd fd(::socket(AF_INET, type | SOCK_CLOEXEC, 0));
  if (!fd) fail("socket");
  return fd;
}

void bind_to(int fd, const std::string& local) {
  const sockaddr_in address = parse_address(local);
  const int on = 1;
  setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &on, sizeof(on));
  if (::bind(fd, reinterpret_cast<const sockaddr*>(&address), sizeof(address)) != 0) fail("bind " + local);
}

// ── UDP ───────────────────────────────────────────────────────────────

class UdpLink final : public HostLink {
 public:
  UdpLink(const std::string& local, const std::string& remote) : socket_(open_socket(SOCK_DGRAM | SOCK_NONBLOCK)) {
    bind_to(socket_.get(), local);
    if (!remote.empty()) {
      remote_ = parse_address(remote);
      fixed_remote_ = has_remote_ = true;
    }
  }

  void send(const Frame& frame) override {
    if (!has_remote_) return;  // nobody to send to yet
    ::sendto(socket_.get(), frame.data(), frame.size(), 0, reinterpret_cast<const sockaddr*>(&remote_),
             sizeof(remote_));
  }

  std::vector<Frame> receive() override {
    std::vector<Frame> frames;
    for (;;) {
      Frame frame(65536);
      sockaddr_in sender{};
      socklen_t length = sizeof(sender);
      const ssize_t n = ::recvfrom(socket_.get(), frame.data(), frame.size(), 0,
                                   reinterpret_cast<sockaddr*>(&sender), &length);
      if (n < 0) break;
      frame.resize(static_cast<size_t>(n));
      frames.push_back(std::move(frame));
      if (!fixed_remote_) remote_ = sender, has_remote_ = true;
    }
    return frames;
  }

  std::string address() const override { return bound_address(socket_.get()); }

 private:
  Fd socket_;
  sockaddr_in remote_{};
  bool has_remote_ = false;
  bool fixed_remote_ = false;
};

// ── TCP ───────────────────────────────────────────────────────────────

// Frames on a stream, each preceded by its length (2 bytes, big endian).
class TcpLink final : public HostLink {
 public:
  static std::unique_ptr<TcpLink> listening(const std::string& address) {
    auto link = std::unique_ptr<TcpLink>(new TcpLink);
    link->listener_ = open_socket(SOCK_STREAM | SOCK_NONBLOCK);
    bind_to(link->listener_.get(), address);
    if (::listen(link->listener_.get(), 1) != 0) fail("listen " + address);
    return link;
  }

  static std::unique_ptr<TcpLink> connected(const std::string& address) {
    auto link = std::unique_ptr<TcpLink>(new TcpLink);
    link->peer_ = open_socket(SOCK_STREAM);
    const sockaddr_in remote = parse_address(address);
    if (::connect(link->peer_.get(), reinterpret_cast<const sockaddr*>(&remote), sizeof(remote)) != 0) {
      fail("connect " + address);
    }
    return link;
  }

  void send(const Frame& frame) override {
    accept_client();
    if (!peer_) return;
    if (frame.size() > 0xFFFF) throw std::length_error("TCP link frames are at most 65535 bytes");
    Frame message{static_cast<uint8_t>(frame.size() >> 8), static_cast<uint8_t>(frame.size())};
    message.insert(message.end(), frame.begin(), frame.end());
    size_t sent = 0;
    while (sent < message.size()) {
      const ssize_t n = ::send(peer_.get(), message.data() + sent, message.size() - sent, MSG_NOSIGNAL);
      if (n <= 0) {
        drop_client();
        return;
      }
      sent += static_cast<size_t>(n);
    }
  }

  std::vector<Frame> receive() override {
    accept_client();
    if (!peer_) return {};
    uint8_t buffer[4096];
    for (;;) {
      const ssize_t n = ::recv(peer_.get(), buffer, sizeof(buffer), MSG_DONTWAIT);
      if (n == 0 || (n < 0 && errno != EAGAIN && errno != EWOULDBLOCK)) {
        drop_client();
        break;
      }
      if (n < 0) break;
      pending_.insert(pending_.end(), buffer, buffer + n);
    }
    std::vector<Frame> frames;
    size_t offset = 0;
    while (pending_.size() - offset >= 2) {
      const size_t length = static_cast<size_t>(pending_[offset]) << 8 | pending_[offset + 1];
      if (pending_.size() - offset - 2 < length) break;
      frames.emplace_back(pending_.begin() + offset + 2, pending_.begin() + offset + 2 + length);
      offset += 2 + length;
    }
    pending_.erase(pending_.begin(), pending_.begin() + offset);
    return frames;
  }

  std::string address() const override { return listener_ ? bound_address(listener_.get()) : std::string(); }

 private:
  TcpLink() = default;

  void accept_client() {
    if (peer_ || !listener_) return;
    peer_.reset(::accept4(listener_.get(), nullptr, nullptr, SOCK_CLOEXEC));
  }

  void drop_client() {
    peer_.reset();
    pending_.clear();
  }

  Fd listener_;
  Fd peer_;
  Frame pending_;
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
      Frame frame(65536);
      const ssize_t n = ::read(tap_.get(), frame.data(), frame.size());
      if (n <= 0) break;
      frame.resize(static_cast<size_t>(n));
      frames.push_back(std::move(frame));
    }
    return frames;
  }

 private:
  Fd tap_;
};
#endif

}  // namespace

std::unique_ptr<HostLink> udp_link(const std::string& local, const std::string& remote) {
  return std::make_unique<UdpLink>(local, remote);
}

std::unique_ptr<HostLink> tcp_listen_link(const std::string& address) { return TcpLink::listening(address); }

std::unique_ptr<HostLink> tcp_connect_link(const std::string& address) { return TcpLink::connected(address); }

std::unique_ptr<HostLink> tap_link(const std::string& interface) {
#ifdef __linux__
  return std::make_unique<TapLink>(interface);
#else
  (void)interface;
  throw std::runtime_error("TAP links need Linux");
#endif
}

}  // namespace costar
