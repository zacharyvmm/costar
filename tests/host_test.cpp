#include <doctest/doctest.h>

#include <arpa/inet.h>
#include <netinet/in.h>
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>

#include <chrono>
#include <string>
#include <system_error>

#include "costar/session.hpp"
#include "costar/world.hpp"

using costar::DeviceKind;
using costar::Frame;
using costar::milliseconds;
using costar::World;

namespace {

// The host's side of a link: a plain socket, closed on scope exit.
struct Socket {
  int fd;
  explicit Socket(int type) : fd(::socket(AF_INET, type, 0)) { REQUIRE(fd >= 0); }
  ~Socket() { ::close(fd); }
};

sockaddr_in address_of(const std::string& text) {
  const size_t colon = text.rfind(':');
  sockaddr_in address{};
  address.sin_family = AF_INET;
  inet_pton(AF_INET, text.substr(0, colon).c_str(), &address.sin_addr);
  address.sin_port = htons(static_cast<uint16_t>(std::stoul(text.substr(colon + 1))));
  return address;
}

// Read whatever arrives within 1 s.
Frame read_some(int fd) {
  pollfd p{fd, POLLIN, 0};
  if (::poll(&p, 1, 1000) != 1) return {};
  Frame buffer(1024);
  const ssize_t n = ::recv(fd, buffer.data(), buffer.size(), 0);
  buffer.resize(n > 0 ? static_cast<size_t>(n) : 0);
  return buffer;
}

// A FreeRTOS link node answering pings ("P", n -> "R", n) on its "link".
World& add_responder(World& world) {
  world.add_machine(LINK_NODE, {{.name = "link", .kind = DeviceKind::Channel, .irq = 1},
                                {.name = "node", .kind = DeviceKind::Registers}})
      .set_register("node", 0, 1);
  return world;
}

}  // namespace

TEST_CASE("a UDP bridge connects firmware to a host socket") {
  World world;
  add_responder(world);
  world.bridge({0, "link"}, costar::udp_link("127.0.0.1:0", ""));
  const std::string bridge = world.bridge_addresses().at(0).second;

  Socket host(SOCK_DGRAM);
  const sockaddr_in to = address_of(bridge);
  const uint8_t ping[2] = {'P', 7};
  REQUIRE(::sendto(host.fd, ping, 2, 0, reinterpret_cast<const sockaddr*>(&to), sizeof(to)) == 2);

  world.run_realtime(milliseconds(50));
  CHECK(read_some(host.fd) == Frame{'R', 7});  // answered to the last sender
}

TEST_CASE("a TCP bridge carries length-prefixed frames, listening or connecting") {
  SUBCASE("listening") {
    World world;
    add_responder(world);
    world.bridge({0, "link"}, costar::tcp_listen_link("127.0.0.1:0"));

    Socket host(SOCK_STREAM);
    const sockaddr_in to = address_of(world.bridge_addresses().at(0).second);
    REQUIRE(::connect(host.fd, reinterpret_cast<const sockaddr*>(&to), sizeof(to)) == 0);
    const uint8_t ping[4] = {0, 2, 'P', 3};
    REQUIRE(::send(host.fd, ping, 4, 0) == 4);

    world.run_realtime(milliseconds(50));
    CHECK(read_some(host.fd) == Frame{0, 2, 'R', 3});
  }
  SUBCASE("connecting") {
    Socket listener(SOCK_STREAM);
    sockaddr_in any = address_of("127.0.0.1:0");
    REQUIRE(::bind(listener.fd, reinterpret_cast<const sockaddr*>(&any), sizeof(any)) == 0);
    REQUIRE(::listen(listener.fd, 1) == 0);
    socklen_t length = sizeof(any);
    ::getsockname(listener.fd, reinterpret_cast<sockaddr*>(&any), &length);

    World world;
    add_responder(world);
    world.bridge({0, "link"}, costar::tcp_connect_link("127.0.0.1:" + std::to_string(ntohs(any.sin_port))));
    const int host = ::accept(listener.fd, nullptr, nullptr);
    REQUIRE(host >= 0);
    const uint8_t ping[4] = {0, 2, 'P', 9};
    REQUIRE(::send(host, ping, 4, 0) == 4);

    world.run_realtime(milliseconds(50));
    CHECK(read_some(host) == Frame{0, 2, 'R', 9});
    ::close(host);
  }
}

TEST_CASE("real-time runs keep pace with the wall clock") {
  World world;
  add_responder(world);
  const auto start = std::chrono::steady_clock::now();
  world.run_realtime(milliseconds(30));
  CHECK(std::chrono::steady_clock::now() - start >= std::chrono::milliseconds(30));
  CHECK(world.now() == milliseconds(30));
}

TEST_CASE("a real-time session keeps pace with the wall clock") {
  costar::Scenario scenario;
  scenario.machines.push_back({.name = "idle", .image = std::nullopt, .devices = {}});
  costar::Session session(scenario, {});
  const auto start = std::chrono::steady_clock::now();
  CHECK(session.run(milliseconds(30), true).realtime);
  while (session.status().running) std::this_thread::sleep_for(std::chrono::milliseconds(1));
  CHECK(std::chrono::steady_clock::now() - start >= std::chrono::milliseconds(30));
  CHECK(session.status().now == milliseconds(30));
}

TEST_CASE("a TAP bridge without the rights to the interface fails clearly") {
  CHECK_THROWS_AS(costar::tap_link("costar-test0"), std::system_error);
}
