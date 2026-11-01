#include "costar/host.hpp"

#include <arpa/inet.h>
#include <poll.h>
#include <fcntl.h>
#include <limits.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <termios.h>
#include <unistd.h>

#include <algorithm>
#include <cerrno>
#include <charconv>
#include <cstdlib>
#include <cstring>
#include <filesystem>
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

// A from_chars result that read a valid number up to `end`.
bool parsed_whole(std::from_chars_result result, const char* end) {
  return result.ec == std::errc() && result.ptr == end;
}

// "a.b.c.d:port" (IPv4), with a port of 0..65535.
sockaddr_in parse_address(const std::string& text) {
  const size_t colon = text.rfind(':');
  sockaddr_in address{};
  address.sin_family = AF_INET;
  unsigned port = 0;
  const char* digits = colon == std::string::npos ? nullptr : text.data() + colon + 1;
  const char* end = text.data() + text.size();
  if (digits == nullptr || inet_pton(AF_INET, text.substr(0, colon).c_str(), &address.sin_addr) != 1 ||
      digits == end || !parsed_whole(std::from_chars(digits, end, port), end) || port > 65535) {
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

// A byte stream's input as frames of at most kMaxFrame bytes.
std::vector<Frame> raw_frames(Frame& input) {
  std::vector<Frame> frames;
  for (size_t offset = 0; offset < input.size(); offset += kMaxFrame) {
    frames.emplace_back(input.begin() + offset, input.begin() + std::min(input.size(), offset + kMaxFrame));
  }
  input.clear();
  return frames;
}

// Write as much of `output` to the non-blocking `fd` as it takes now, and
// keep the rest.  False if the write failed (errno says why).
bool write_some(int fd, Frame& output, bool socket) {
  size_t sent = 0;
  bool ok = true;
  while (sent < output.size()) {
    const ssize_t n = socket ? ::send(fd, output.data() + sent, output.size() - sent, MSG_NOSIGNAL)
                             : ::write(fd, output.data() + sent, output.size() - sent);
    if (n > 0) {
      sent += static_cast<size_t>(n);
    } else if (n < 0 && errno == EINTR) {
      continue;
    } else {
      ok = n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK);
      break;
    }
  }
  output.erase(output.begin(), output.begin() + sent);
  return ok;
}

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

// Frames on a stream, each preceded by its length (2 bytes, big endian), or
// a raw byte stream.  Every socket is non-blocking: output waits in a queue
// (kept across partial writes) so a peer that stops reading never stalls the
// simulation.
class TcpLink final : public HostLink {
 public:
  static std::unique_ptr<TcpLink> listening(const std::string& address, Framing framing) {
    auto link = std::unique_ptr<TcpLink>(new TcpLink(address, framing));
    link->listener_ = open_socket(SOCK_STREAM);
    bind_to(link->listener_.get(), address, true);
    if (::listen(link->listener_.get(), 1) != 0) fail("listen " + address);
    return link;
  }

  // Connects in the background; a failure surfaces on a later poll.
  static std::unique_ptr<TcpLink> connecting(const std::string& address, Framing framing) {
    auto link = std::unique_ptr<TcpLink>(new TcpLink(address, framing));
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
    const bool length = framing_ == Framing::Length;
    if (length && frame.size() > 0xFFFF) throw std::length_error("TCP link frames are at most 65535 bytes");
    update_peer();
    if (!peer_) return;  // no client yet: nobody to send to
    if (output_.size() + frame.size() + 2 > kMaxQueued) {
      if (listener_) {
        drop_peer();  // a client that stopped reading; wait for the next
        return;
      }
      throw std::runtime_error("TCP link to " + address_ + ": the peer is not reading");
    }
    if (length) {
      output_.push_back(static_cast<uint8_t>(frame.size() >> 8));
      output_.push_back(static_cast<uint8_t>(frame.size()));
    }
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

  TcpLink(std::string address, Framing framing) : address_(std::move(address)), framing_(framing) {}

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
    if (!write_some(peer_.get(), output_, true)) lost("send failed");
  }

  std::vector<Frame> decode() {
    if (framing_ == Framing::Raw) return raw_frames(input_);
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
  Framing framing_;
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

// ── Pseudo-terminal ───────────────────────────────────────────────────

// costar holds the terminal's own side open too, so the pty stays usable
// while no program is attached (reads would fail otherwise) and a program
// can leave and come back.
class PtyLink final : public HostLink {
 public:
  explicit PtyLink(const std::string& link) : master_(::posix_openpt(O_RDWR | O_NOCTTY | O_CLOEXEC)) {
    if (!master_) fail("open a pseudo-terminal");
    char path[128];
    if (::grantpt(master_.get()) != 0 || ::unlockpt(master_.get()) != 0 ||
        ::ptsname_r(master_.get(), path, sizeof(path)) != 0) {
      fail("set up a pseudo-terminal");
    }
    path_ = path;
    terminal_.reset(::open(path, O_RDWR | O_NOCTTY | O_CLOEXEC));
    if (!terminal_) fail("open " + path_);
    termios mode{};
    if (::tcgetattr(terminal_.get(), &mode) != 0) fail("tcgetattr " + path_);
    ::cfmakeraw(&mode);
    if (::tcsetattr(terminal_.get(), TCSANOW, &mode) != 0) fail("tcsetattr " + path_);
    if (::fcntl(master_.get(), F_SETFL, O_NONBLOCK) != 0) fail("fcntl " + path_);
    if (!link.empty()) make_link(link);
  }
  ~PtyLink() override {
    if (link_.empty()) return;
    std::error_code error;
    if (std::filesystem::read_symlink(link_, error) == path_) std::filesystem::remove(link_, error);
  }

  void send(const Frame& frame) override {
    if (output_.size() + frame.size() > kMaxQueued) return;  // nobody is reading
    output_.insert(output_.end(), frame.begin(), frame.end());
    write_some(master_.get(), output_, false);
  }

  std::vector<Frame> receive() override {
    write_some(master_.get(), output_, false);
    for (;;) {
      const ssize_t n = ::read(master_.get(), buffer_.data(), buffer_.size());
      if (n < 0 && errno == EINTR) continue;
      if (n <= 0) break;
      input_.insert(input_.end(), buffer_.begin(), buffer_.begin() + n);
    }
    return raw_frames(input_);
  }

  std::string address() const override { return path_; }

 private:
  static constexpr size_t kMaxQueued = 1 << 20;

  // A symbolic link at `link` to the terminal.  A stale one from an earlier
  // run is replaced; a file is not.
  void make_link(const std::string& link) {
    std::error_code error;
    const auto status = std::filesystem::symlink_status(link, error);
    if (std::filesystem::exists(status) && !std::filesystem::is_symlink(status)) {
      throw std::runtime_error("pty link " + link + " exists and is not a symbolic link");
    }
    const std::string temporary = link + ".costar-" + std::to_string(::getpid());
    if (::symlink(path_.c_str(), temporary.c_str()) != 0) fail("symlink " + temporary);
    if (::rename(temporary.c_str(), link.c_str()) != 0) {
      const int saved = errno;
      ::unlink(temporary.c_str());
      errno = saved;
      fail("rename " + temporary + " to " + link);
    }
    link_ = link;
  }

  Fd master_;
  Fd terminal_;
  std::string path_;
  std::string link_;
  Frame buffer_ = Frame(kMaxFrame);
  Frame input_;
  Frame output_;
};

}  // namespace

// ── Streams on file descriptors ───────────────────────────────────────

// `out` may be shared with other programs too, so its flags stay as they are
// (blocking, usually).  A terminal is opened again, non-blocking: that's a
// file description of costar's own, so a write to a terminal that takes no
// more (stopped with Ctrl-S, say) returns at once instead of blocking.  It's
// opened through /proc/self/fd, which opens the very file `out` is (a name
// from ttyname() may name another), and checked to be the same file and
// terminal.  An alias for a terminal (/dev/tty, /dev/console, /dev/tty0)
// isn't opened again: it names whichever terminal it stands for now, which
// may not be `out`'s.  A terminal that can't be opened again is an error: a
// blocking write could keep a run from stopping.
StreamLink::StreamLink(int in, int out, std::optional<uint8_t> escape, std::function<bool()> stopping)
    : in_(in), out_(out), escape_(escape), stopping_(std::move(stopping)) {
  if (!::isatty(out)) return;
  const auto fail = [](const std::string& why) {
    throw std::runtime_error("can't open the console's terminal again, non-blocking: " + why);
  };
  struct stat shared{};
  if (::fstat(out, &shared) != 0) fail(std::strerror(errno));
  if (major(shared.st_rdev) == 5 || shared.st_rdev == makedev(4, 0)) {
    fail("standard output is an alias for a terminal (/dev/tty, /dev/console, /dev/tty0); use the terminal itself");
  }
  const std::string path = "/proc/self/fd/" + std::to_string(out);
  own_out_ = ::open(path.c_str(), O_WRONLY | O_NOCTTY | O_NONBLOCK | O_CLOEXEC);
  if (own_out_ < 0) fail(path + ": " + std::strerror(errno));
  struct stat own{};
  unsigned shared_tty = 0, own_tty = 0;  // the terminals' device numbers
  if (::fstat(own_out_, &own) != 0 || own.st_dev != shared.st_dev || own.st_ino != shared.st_ino ||
      ::ioctl(out, TIOCGDEV, &shared_tty) != 0 || ::ioctl(own_out_, TIOCGDEV, &own_tty) != 0 ||
      own_tty != shared_tty) {
    ::close(own_out_);
    own_out_ = -1;
    fail(path + ": not the same terminal");
  }
  out_ = own_out_;
}

StreamLink::~StreamLink() {
  if (own_out_ >= 0) ::close(own_out_);
}

// Each write waits in poll() first, waking now and then to see if the run is
// stopping, and writes at most PIPE_BUF bytes, which a pipe that poll() calls
// writable takes at once (on Linux); a terminal's writes don't block (see
// above).  So a stop is seen within kStopCheckMs, and drops the rest of the
// output.
void StreamLink::send(const Frame& frame) {
  for (size_t done = 0; done < frame.size() && !closed_;) {
    if (stopping_ && stopping_()) return;
    pollfd p{out_, POLLOUT, 0};
    const int ready = ::poll(&p, 1, kStopCheckMs);
    if (ready == 0 || (ready < 0 && errno == EINTR)) continue;
    const size_t chunk = std::min<size_t>(frame.size() - done, PIPE_BUF);
    const ssize_t n = ready < 0 ? -1 : ::write(out_, frame.data() + done, chunk);
    if (n < 0 && (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK)) continue;
    if (n <= 0) {
      closed_ = true;  // nobody reads it any more (EPIPE), or it broke
      return;
    }
    done += static_cast<size_t>(n);
    written_ += static_cast<uint64_t>(n);
  }
}

// One read per poll, and only once poll() says it won't block: `in` may be
// shared with other programs (a terminal), so it is never made non-blocking.
std::vector<Frame> StreamLink::receive() {
  pollfd p{in_, POLLIN, 0};
  if (ended_ || ::poll(&p, 1, 0) != 1) return {};
  Frame buffer(kMaxFrame);
  ssize_t n;
  do {
    n = ::read(in_, buffer.data(), buffer.size());
  } while (n < 0 && errno == EINTR);
  if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) return {};
  if (n <= 0) {
    ended_ = true;
    return {};
  }
  buffer.resize(static_cast<size_t>(n));
  if (escape_) {
    if (const auto at = std::ranges::find(buffer, *escape_); at != buffer.end()) {
      buffer.erase(at, buffer.end());
      ended_ = escaped_ = true;
    }
  }
  if (buffer.empty()) return {};
  return {std::move(buffer)};
}

std::unique_ptr<HostLink> udp_link(const std::string& local, const std::string& remote) {
  return std::make_unique<UdpLink>(local, remote);
}

std::unique_ptr<HostLink> tcp_listen_link(const std::string& address, Framing framing) {
  return TcpLink::listening(address, framing);
}

std::unique_ptr<HostLink> tcp_connect_link(const std::string& address, Framing framing) {
  return TcpLink::connecting(address, framing);
}

std::unique_ptr<HostLink> pty_link(const std::string& link) { return std::make_unique<PtyLink>(link); }

std::unique_ptr<HostLink> tap_link(const std::string& interface) {
#ifdef __linux__
  return std::make_unique<TapLink>(interface);
#else
  (void)interface;
  throw std::runtime_error("TAP links need Linux");
#endif
}

}  // namespace costar
