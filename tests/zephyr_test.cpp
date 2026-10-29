#include <doctest/doctest.h>

#include <string>
#include <utility>
#include <vector>

#include "costar/world.hpp"

using costar::milliseconds;
using costar::Time;
using costar::World;

namespace {

using ConsoleLine = std::pair<Time, std::string>;

std::vector<ConsoleLine> console(const World& world, uint32_t machine) {
  std::vector<ConsoleLine> out;
  for (const auto& r : world.trace().for_machine(machine)) {
    if (r.label == "log") out.emplace_back(r.at, r.text.value());
  }
  return out;
}

// What firmware/zephyr_ping_pong prints on stock native_sim (`zephyr.exe`),
// each line at the uptime it reports.  k_msleep(3) sleeps 4 ticks: Zephyr
// rounds up by one tick.
std::vector<ConsoleLine> expected_zephyr_ping_pong() {
  std::vector<ConsoleLine> out;
  for (int i = 1; i <= 5; ++i) {
    const int ms = (i - 1) * 4;
    out.emplace_back(milliseconds(ms), "received " + std::to_string(i) + " at " + std::to_string(ms) + " ms");
    out.emplace_back(milliseconds(ms), "sent " + std::to_string(i) + " at " + std::to_string(ms) + " ms");
  }
  out.emplace_back(milliseconds(20), "sender done at 20 ms");
  out.emplace_back(milliseconds(20), "exit task ran");  // the app's NATIVE_TASK(..., ON_EXIT)
  return out;
}

}  // namespace

TEST_CASE("unmodified Zephyr (native_sim) behaves as on Zephyr's own runner") {
  World world;
  world.add_machine(ZEPHYR_PING_PONG);
  world.run_until(milliseconds(50));
  CHECK(console(world, 0) == expected_zephyr_ping_pong());
}

TEST_CASE("two copies of one Zephyr image are independent kernels") {
  World world;
  world.add_machine(ZEPHYR_PING_PONG);
  world.add_machine(ZEPHYR_PING_PONG);
  world.run_until(milliseconds(50));
  CHECK(console(world, 0) == expected_zephyr_ping_pong());
  CHECK(console(world, 1) == expected_zephyr_ping_pong());
}

TEST_CASE("Zephyr and FreeRTOS machines share one world") {
  World world;
  world.add_machine(ZEPHYR_PING_PONG);
  world.add_machine(PING_PONG_3);
  world.run_until(milliseconds(50));
  CHECK(console(world, 0) == expected_zephyr_ping_pong());
  CHECK(world.trace().for_machine(1).size() == 11);  // the FreeRTOS ping-pong trace
}

TEST_CASE("a Zephyr program that exits runs native_sim's exit tasks, then stops with its status") {
  World world;
  world.add_machine(ZEPHYR_PING_PONG);
  world.run();
  CHECK(console(world, 0) == expected_zephyr_ping_pong());
  CHECK(world.machine(0).cpu().state() == costar::Cpu::State::Exited);
  // main() exits with 3; an exit task calls nsi_exit(0) again; 3 wins.
  CHECK(world.trace().records().back() == costar::TraceRecord{milliseconds(20), 0, "stop", 3});
}

TEST_CASE("Zephyr worlds can be torn down mid-run, repeatedly") {
  for (int i = 0; i < 20; ++i) {
    World world;
    world.add_machine(ZEPHYR_PING_PONG);
    world.add_machine(ZEPHYR_PING_PONG);
    world.run_until(milliseconds(6));
  }
  CHECK(true);
}

TEST_CASE("Zephyr drivers put costar devices behind Zephyr's UART and flash APIs") {
  World world;
  world.add_machine(ZEPHYR_UART_FLASH, {{.name = "uart", .kind = costar::DeviceKind::Channel, .irq = 5},
                                        {.name = "flash", .kind = costar::DeviceKind::Memory, .size = 1024}});
  costar::Machine& host = world.add_passive_machine({{.name = "uart", .kind = costar::DeviceKind::Channel}});
  world.connect(costar::Wire{.endpoints = {{0, "uart"}, {1, "uart"}}});
  world.at(milliseconds(5), [&] { host.send("uart", {'o', 'k'}); });
  world.run_until(milliseconds(10));

  // The first boot counts 1 in flash (erased flash reads 0xff).
  const costar::Frame& flash = world.machine(0).devices().get<costar::Memory>("flash").bytes;
  CHECK(costar::Frame(flash.begin(), flash.begin() + 4) == costar::Frame{1, 0, 0, 0});
  CHECK(costar::Frame(flash.begin() + 4, flash.end()) == costar::Frame(1020, 0xff));

  // The boot greeting, one byte per TX interrupt, then an interrupt-driven
  // echo, upper-cased, at the instant the frame arrives.  uart_poll_out()
  // sends one byte per call, so the echo is two frames.
  std::vector<std::pair<Time, std::string>> received;
  for (const auto& r : world.trace().for_machine(1)) {
    if (r.label == "rx") received.emplace_back(r.at, r.text.value());
  }
  CHECK(received == std::vector<std::pair<Time, std::string>>{
                        {0, "uart 72"}, {0, "uart 65"}, {0, "uart 61"}, {0, "uart 64"}, {0, "uart 79"},
                        {0, "uart 0a"}, {milliseconds(5), "uart 4f"}, {milliseconds(5), "uart 4b"}});
}

TEST_CASE("a costar UART carries frames longer than 256 bytes whole") {
  World world;
  world.add_machine(ZEPHYR_UART_FLASH, {{.name = "uart", .kind = costar::DeviceKind::Channel, .irq = 5},
                                        {.name = "flash", .kind = costar::DeviceKind::Memory, .size = 1024}});
  costar::Machine& host = world.add_passive_machine({{.name = "uart", .kind = costar::DeviceKind::Channel}});
  world.connect(costar::Wire{.endpoints = {{0, "uart"}, {1, "uart"}}});
  world.at(milliseconds(5), [&] { host.send("uart", costar::Frame(300, 'a')); });
  world.run_until(milliseconds(10));

  int echoed = 0;
  for (const auto& r : world.trace().for_machine(1)) {
    if (r.label == "rx" && r.text == "uart 41") ++echoed;  // 'A'
  }
  CHECK(echoed == 300);
}
