#include <doctest/doctest.h>

#include <arpa/inet.h>
#include <netinet/in.h>
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>

#include <chrono>
#include <ctime>
#include <thread>
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

TEST_CASE("a TCP peer that stops reading does not stall the simulation") {
  World world;
  world.add_passive_machine({{.name = "link", .kind = DeviceKind::Channel}});
  world.bridge({0, "link"}, costar::tcp_listen_link("127.0.0.1:0"));
  Socket host(SOCK_STREAM);
  const sockaddr_in to = address_of(world.bridge_addresses().at(0).second);
  REQUIRE(::connect(host.fd, reinterpret_cast<const sockaddr*>(&to), sizeof(to)) == 0);  // and never reads

  world.run_realtime(milliseconds(5));  // the bridge accepts the client
  const auto start = std::chrono::steady_clock::now();
  for (int i = 0; i < 200; ++i) world.machine(0).send("link", Frame(60'000, 'x'));  // ~12 MB
  world.run_realtime(milliseconds(25));
  // A blocking write would never return.  The bound is loose because tracing
  // 12 MB of frames takes seconds in sanitizer builds.
  CHECK(std::chrono::steady_clock::now() - start < std::chrono::seconds(30));
}

TEST_CASE("a TCP connection is made in the background, and its failure is reported") {
  const auto start = std::chrono::steady_clock::now();
  auto unreachable = costar::tcp_connect_link("192.0.2.1:9");  // TEST-NET-1: never answers
  CHECK(std::chrono::steady_clock::now() - start < std::chrono::milliseconds(500));

  // A local port nobody listens on refuses at once.
  Socket probe(SOCK_STREAM);
  sockaddr_in any = address_of("127.0.0.1:0");
  REQUIRE(::bind(probe.fd, reinterpret_cast<const sockaddr*>(&any), sizeof(any)) == 0);
  socklen_t length = sizeof(any);
  ::getsockname(probe.fd, reinterpret_cast<sockaddr*>(&any), &length);
  auto refused = costar::tcp_connect_link("127.0.0.1:" + std::to_string(ntohs(any.sin_port)));
  bool reported = false;
  for (int i = 0; i < 100 && !reported; ++i) {
    try {
      refused->receive();
      std::this_thread::sleep_for(std::chrono::milliseconds(10));
    } catch (const std::system_error&) {
      reported = true;
    }
  }
  CHECK(reported);
}

TEST_CASE("frames sent before the peer closes its side still arrive, and the reply goes back") {
  World world;
  add_responder(world);
  world.bridge({0, "link"}, costar::tcp_listen_link("127.0.0.1:0"));
  Socket host(SOCK_STREAM);
  const sockaddr_in to = address_of(world.bridge_addresses().at(0).second);
  REQUIRE(::connect(host.fd, reinterpret_cast<const sockaddr*>(&to), sizeof(to)) == 0);
  const uint8_t ping[4] = {0, 2, 'P', 4};
  REQUIRE(::send(host.fd, ping, 4, 0) == 4);
  ::shutdown(host.fd, SHUT_WR);

  world.run_realtime(milliseconds(50));
  CHECK(read_some(host.fd) == Frame{0, 2, 'R', 4});
}

TEST_CASE("an idle real-time session sleeps instead of spinning") {
  costar::Scenario scenario;
  scenario.machines.push_back({.name = "idle", .image = std::nullopt, .devices = {}});
  costar::Session session(scenario, {});
  const std::clock_t cpu_start = std::clock();
  session.run(milliseconds(300), true);
  while (session.status().running) std::this_thread::sleep_for(std::chrono::milliseconds(5));
  const double cpu_seconds = double(std::clock() - cpu_start) / CLOCKS_PER_SEC;
  CHECK(cpu_seconds < 0.1);  // spinning would use ~0.3 s
}

TEST_CASE("a session with host bridges refuses to seek") {
  costar::Session session(costar::parse_scenario(R"(
[[machine]]
name = "m"
devices = [{ name = "link", kind = "channel" }]
[[bridge]]
endpoint = "m.link"
kind = "udp"
local = "127.0.0.1:0"
)", "bridged.toml"), {});
  CHECK_THROWS_AS(session.seek(0), std::invalid_argument);
}

TEST_CASE("UDP bridges bind exclusively, and addresses are parsed strictly") {
  const auto first = costar::udp_link("127.0.0.1:0", "");
  CHECK_THROWS_AS(costar::udp_link(first->address(), ""), std::system_error);
  for (const char* bad : {"127.0.0.1:65536", "127.0.0.1:4294967296", "127.0.0.1:99999999999999999999",
                          "127.0.0.1:0junk", "127.0.0.1:-1", "127.0.0.1:", "localhost:80"}) {
    CHECK_THROWS_AS(costar::udp_link(bad, ""), std::invalid_argument);
  }
}
