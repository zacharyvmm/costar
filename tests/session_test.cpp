#include <doctest/doctest.h>

#include <algorithm>
#include <chrono>
#include <filesystem>
#include <string>
#include <thread>

#include "costar/session.hpp"
#include "costar/world.hpp"

using costar::Input;
using costar::milliseconds;
using costar::Scenario;
using costar::Session;
using costar::Time;
using costar::TraceRecord;

namespace {

Scenario scenario(const std::string& text) {
  return costar::parse_scenario(text, "test.toml", {std::filesystem::path(LINK_NODE).parent_path()});
}

const char* kLinkNodes = R"(
[[machine]]
name = "initiator"
image = "freertos_link_node.so"
devices = [{ name = "link", kind = "channel", irq = 1 }, { name = "node", kind = "registers" }]
[[machine]]
name = "responder"
image = "freertos_link_node.so"
devices = [{ name = "link", kind = "channel", irq = 1 }, { name = "node", kind = "registers" }]
[[wire]]
endpoints = ["initiator.link", "responder.link"]
latency_ms = 2
[[input]]
target = "responder.node"
register = 0
value = 1
)";

const char* kPassivePair = R"(
[[machine]]
name = "a"
devices = [{ name = "link", kind = "channel" }, { name = "gpio", kind = "registers" }]
[[machine]]
name = "b"
devices = [{ name = "link", kind = "channel" }, { name = "flash", kind = "memory", size = 8 }]
[[wire]]
endpoints = ["a.link", "b.link"]
latency_ms = 1
)";

costar::SessionStatus wait_until_stopped(Session& session) {
  const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(10);
  for (;;) {
    const costar::SessionStatus status = session.status();
    if (!status.running || std::chrono::steady_clock::now() > deadline) return status;
    std::this_thread::sleep_for(std::chrono::milliseconds(1));
  }
}

std::vector<TraceRecord> whole_trace(Session& session) {
  Session::TraceCursor cursor;
  return *session.wait_trace(cursor, std::chrono::milliseconds(0));
}

Input send(Time at, const char* text) {
  return {.kind = Input::Kind::Send, .at = at, .target = {0, "link"}, .data = costar::Frame(text, text + 4)};
}

}  // namespace

TEST_CASE("a session runs in slices and matches an uninterrupted run") {
  Session session(scenario(kLinkNodes), {});
  session.run(milliseconds(30));
  const costar::SessionStatus status = wait_until_stopped(session);
  CHECK_FALSE(status.running);
  CHECK(status.now == milliseconds(30));

  costar::World reference;
  costar::build_world(scenario(kLinkNodes), reference);
  reference.run_until(milliseconds(30));
  CHECK(whole_trace(session) == reference.trace().records());
}

TEST_CASE("a quiescent session stops by itself") {
  Session session(scenario(kLinkNodes), {});
  session.run(costar::kNever);
  const costar::SessionStatus status = wait_until_stopped(session);
  CHECK_FALSE(status.running);
  // The initiator wakes from its last delay at 30 ms and deletes itself;
  // after that every machine waits for an interrupt that never comes.
  CHECK(status.now == milliseconds(30));
}

TEST_CASE("pause stops a running session") {
  Session session(scenario(kLinkNodes), {});
  session.run(costar::kNever);
  const costar::SessionStatus paused = session.pause();
  CHECK_FALSE(paused.running);
  CHECK(session.status().now == paused.now);
}

TEST_CASE("inputs are recorded, and seek replays them exactly") {
  Session session(scenario(kPassivePair), {});
  session.input(send(milliseconds(5), "ping"));
  session.input(send(milliseconds(12), "pong"));
  session.run(milliseconds(20));
  wait_until_stopped(session);
  const std::vector<TraceRecord> first = whole_trace(session);
  REQUIRE(first.size() == 4);  // two sends, two receptions

  session.seek(milliseconds(8));  // keeps the 5 ms input, drops the 12 ms one
  CHECK(session.status().now == milliseconds(8));
  CHECK(whole_trace(session) == std::vector<TraceRecord>(first.begin(), first.begin() + 2));

  session.input(send(milliseconds(12), "pong"));
  session.run(milliseconds(20));
  wait_until_stopped(session);
  CHECK(whole_trace(session) == first);
}

TEST_CASE("an input in the past applies now") {
  Session session(scenario(kPassivePair), {});
  session.run(milliseconds(10));
  wait_until_stopped(session);
  session.input(send(0, "late"));
  session.run(milliseconds(20));
  wait_until_stopped(session);
  CHECK(whole_trace(session).front().at == milliseconds(10));
}

TEST_CASE("watchers follow the trace live and learn about rewinds") {
  Session session(scenario(kLinkNodes), {});
  Session::TraceCursor cursor;
  CHECK(session.wait_trace(cursor, std::chrono::milliseconds(0))->empty());  // nothing has run yet

  session.run(milliseconds(30));
  size_t seen = 0;
  while (seen < 19) {  // the role register, then six records per ping
    const auto records = session.wait_trace(cursor, std::chrono::milliseconds(1000));
    REQUIRE(records.has_value());
    REQUIRE_FALSE(records->empty());
    seen += records->size();
  }
  CHECK(seen == 19);

  session.seek(0);
  CHECK_FALSE(session.wait_trace(cursor, std::chrono::milliseconds(0)).has_value());
}

TEST_CASE("bad inputs are rejected and the session carries on") {
  Session session(scenario(kPassivePair), {});
  Input bad = send(0, "ping");
  bad.target.device = "missing";
  CHECK_THROWS(session.input(bad));
  bad = {.kind = Input::Kind::WriteMemory, .at = 0, .target = {1, "flash"}, .data = costar::Frame(9, 0), .offset = 0};
  CHECK_THROWS(session.input(bad));
  CHECK(session.run(milliseconds(5)).running);
}

TEST_CASE("inspect shows machines and devices") {
  Session session(scenario(kPassivePair), {});
  session.input({.kind = Input::Kind::SetRegister, .at = 0, .target = {0, "gpio"}, .reg = 2, .value = 9});
  session.input({.kind = Input::Kind::WriteMemory, .at = 0, .target = {1, "flash"}, .data = {1, 2}});
  session.input({.kind = Input::Kind::Deliver, .at = 0, .target = {1, "link"}, .data = {7}});
  session.run(milliseconds(1));
  wait_until_stopped(session);

  const costar::Snapshot snapshot = session.inspect();
  REQUIRE(snapshot.machines.size() == 2);
  CHECK(snapshot.machines[0].name == "a");
  CHECK(snapshot.machines[0].cpu_state == "off");
  CHECK(snapshot.machines[0].devices[1].registers.at(2) == 9);
  CHECK(snapshot.machines[1].devices[0].queued_frames == 1);
  CHECK(snapshot.machines[1].devices[1].memory == costar::Frame{1, 2, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff});
}

TEST_CASE("a session whose firmware does not load fails to start") {
  CHECK_THROWS(Session(scenario("[[machine]]\nname = \"m\"\nimage = \"/etc/hostname\""), {}));
}

namespace {

// Writes its step count to a.gpio[0] every millisecond; throws on build
// while *fail is set.
struct Ticker : costar::Plant {
  uint32_t steps = 0;
  void step(Time, costar::World& world) override { world.machine(0).set_register("gpio", 0, ++steps); }
};

costar::PlantFactories ticker_factory(bool* fail) {
  return {{"ticker", [fail](const costar::PlantParams&, const Scenario&) -> std::unique_ptr<costar::Plant> {
             if (*fail) throw std::runtime_error("plant refused to build");
             return std::make_unique<Ticker>();
           }}};
}

const std::string kTicking = std::string(kPassivePair) + R"(
[[plant]]
type = "ticker"
period_ms = 1
)";

uint32_t gpio(Session& session, uint32_t reg) {
  return session.inspect().machines[0].devices[1].registers.at(reg);
}

Input set_gpio(Time at, uint32_t reg, uint32_t value) {
  return {.kind = Input::Kind::SetRegister, .at = at, .target = {0, "gpio"}, .reg = reg, .value = value};
}

}  // namespace

TEST_CASE("seek replays an input after the events that ran before it was submitted") {
  bool fail = false;
  Session session(scenario(kTicking), ticker_factory(&fail));
  session.seek(milliseconds(1));                     // the plant's 1 ms step has run
  session.input(set_gpio(milliseconds(1), 0, 999));  // so this lands after it
  session.run(1'500'000);
  wait_until_stopped(session);
  CHECK(gpio(session, 0) == 999);

  session.seek(1'500'000);
  CHECK(gpio(session, 0) == 999);  // not overwritten by the 1 ms step on replay
}

TEST_CASE("a failed seek keeps the world and its history") {
  bool fail = false;
  Session session(scenario(kTicking), ticker_factory(&fail));
  session.input(set_gpio(milliseconds(5), 1, 9));
  fail = true;
  CHECK_THROWS(session.seek(milliseconds(1)));
  fail = false;

  session.run(milliseconds(10));
  wait_until_stopped(session);
  CHECK(gpio(session, 1) == 9);
  session.seek(milliseconds(10));
  CHECK(gpio(session, 1) == 9);  // the input is still recorded
}

TEST_CASE("an unbounded run stops at the last event, not at a slice boundary") {
  Session session(scenario(kPassivePair), {});
  session.input(send(1'500'000, "ping"));  // delivered 1 ms later
  session.run(costar::kNever);
  CHECK(wait_until_stopped(session).now == 2'500'000);
}

TEST_CASE("a closed session refuses further calls") {
  Session session(scenario(kPassivePair), {});
  session.close();
  session.close();
  CHECK_THROWS(session.status());
}

TEST_CASE("seek keeps inputs queued before the world first ran ahead of its own events") {
  bool fail = false;
  Session session(scenario(kTicking), ticker_factory(&fail));
  session.input(set_gpio(milliseconds(1), 0, 999));  // queued at time 0, before any step
  session.run(1'500'000);
  wait_until_stopped(session);
  const uint32_t before = gpio(session, 0);  // the input ran first at 1 ms, then the plant
  CHECK(before == 2);
  session.seek(1'500'000);
  CHECK(gpio(session, 0) == before);
}

TEST_CASE("seek keeps inputs queued together together") {
  Session session(scenario(R"(
[[machine]]
name = "a"
devices = [{ name = "link", kind = "channel" }]
[[machine]]
name = "b"
devices = [{ name = "link", kind = "channel" }]
[[wire]]
endpoints = ["a.link", "b.link"]
)"),
                  {});
  session.input(send(0, "one!"));
  session.input(send(0, "two!"));
  session.run(milliseconds(1));
  wait_until_stopped(session);
  const std::vector<TraceRecord> first = whole_trace(session);
  CHECK(first.size() == 4);
  session.seek(milliseconds(1));
  CHECK(whole_trace(session) == first);
}

TEST_CASE("seek replays power cycles exactly") {
  Session session(scenario(kLinkNodes), {});
  session.input({.kind = Input::Kind::Power, .at = milliseconds(5), .target = {1, ""}, .power = Input::Power::Off});
  session.input({.kind = Input::Kind::Power, .at = milliseconds(15), .target = {1, ""}, .power = Input::Power::On});
  session.run(milliseconds(40));
  wait_until_stopped(session);
  const std::vector<TraceRecord> first = whole_trace(session);
  CHECK(std::ranges::count(first, TraceRecord{milliseconds(5), 1, "power", 0}) == 1);

  session.seek(milliseconds(20));
  CHECK(session.inspect().machines.at(1).cpu_state != "off");
  session.run(milliseconds(40));
  wait_until_stopped(session);
  CHECK(whole_trace(session) == first);

  session.seek(milliseconds(10));  // drops the power-on at 15 ms
  CHECK(session.inspect().machines.at(1).cpu_state == "off");
}
