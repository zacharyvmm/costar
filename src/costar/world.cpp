#include "costar/world.hpp"

#include <algorithm>
#include <chrono>
#include <stdexcept>
#include <thread>

namespace costar {

// ── Building ──────────────────────────────────────────────────────────

Machine& World::add_machine(const std::filesystem::path& image, const std::vector<DeviceSpec>& devices) {
  const auto id = static_cast<uint32_t>(machines_.size());
  Machine& machine = *machines_.emplace_back(std::make_unique<Machine>(id, image, devices, now_, trace_));
  machine.set_stall_limit(stall_limit_);
  return machine;
}

Machine& World::add_passive_machine(const std::vector<DeviceSpec>& devices) {
  const auto id = static_cast<uint32_t>(machines_.size());
  return *machines_.emplace_back(std::make_unique<Machine>(id, std::nullopt, devices, now_, trace_));
}

void World::set_stall_limit(uint64_t limit) {
  stall_limit_ = limit;
  for (const auto& machine : machines_) machine->set_stall_limit(limit);
}

void World::connect(Wire wire) { wires_.push_back(std::move(wire)); }

void World::add_plant(std::unique_ptr<Plant> plant, Time period) {
  if (period == 0) throw std::invalid_argument("a plant's period must be positive");
  step_plant(*plants_.emplace_back(std::move(plant)), period, now_);
}

void World::at(Time when, EventQueue::Action action) { events_.schedule(when, std::move(action)); }

void World::bridge(Endpoint endpoint, std::unique_ptr<HostLink> link) {
  machine(endpoint.machine).devices().get<Channel>(endpoint.device);  // throws if it is not a channel
  bridges_.emplace_back(std::move(endpoint), std::move(link));
}

// ── Running ───────────────────────────────────────────────────────────

void World::run_until(Time until) {
  run_events_until(until);
  if (until != kNever) now_ = std::max(now_, until);
}

void World::run_events_until(Time until) {
  while (const std::optional<Time> next = next_event_time()) {
    if (*next > until) break;
    step(*next);
  }
}

void World::run_steps(uint64_t count) {
  while (steps_ < count) {
    const std::optional<Time> next = next_event_time();
    if (!next) throw std::logic_error("World::run_steps: nothing left to happen");
    step(*next);
  }
}

void World::idle_until(Time until) {
  const std::optional<Time> next = next_event_time();
  if (next && *next < until) throw std::logic_error("World::idle_until: something is due before then");
  now_ = std::max(now_, until);
}

// One step: everything that happens at `at`.
void World::step(Time at) {
  now_ = at;
  fire_events_at(now_);   // deliveries, plant steps, timers: device state and IRQs
  run_awake_machines();   // each awake CPU runs until it halts or polls
  route_outputs(now_);    // frames the firmware sent go onto wires
  ++steps_;
}

void World::run_realtime(Time until) {
  using Clock = std::chrono::steady_clock;
  const Clock::time_point wall_start = Clock::now();
  const Time virtual_start = now_;
  while (now_ < until) {
    poll_host();
    const auto elapsed = std::chrono::duration_cast<std::chrono::nanoseconds>(Clock::now() - wall_start);
    run_until(std::min(until, virtual_start + static_cast<Time>(elapsed.count())));
    if (now_ < until) std::this_thread::sleep_for(std::chrono::milliseconds(1));
  }
}

std::vector<Stall> World::stalls() const {
  std::vector<Stall> out;
  for (const auto& machine : machines_) {
    for (const Time at : machine->stalls()) out.push_back({at, machine->id()});
  }
  std::ranges::stable_sort(out, {}, &Stall::at);
  return out;
}

std::vector<std::pair<Endpoint, std::string>> World::bridge_addresses() const {
  std::vector<std::pair<Endpoint, std::string>> out;
  for (const auto& [endpoint, link] : bridges_) out.emplace_back(endpoint, link->address());
  return out;
}

void World::poll_host() {
  for (auto& [endpoint, link] : bridges_) {
    for (Frame& frame : link->receive()) machine(endpoint.machine).deliver(endpoint.device, std::move(frame));
  }
}

std::optional<Time> World::next_event_time() const {
  // Frames waiting to be routed (sent by the host between runs, say) need a
  // round now.
  for (const auto& machine : machines_) {
    if (machine->has_outbox()) return now_;
  }
  Time next = events_.next_time().value_or(kNever);
  for (const auto& machine : machines_) next = std::min(next, machine->cpu().next_event());
  if (next == kNever) return std::nullopt;
  return std::max(next, now_);
}

void World::fire_events_at(Time now) {
  events_.run_due(now);
  for (const auto& machine : machines_) machine->cpu().advance_to(now);
}

void World::run_awake_machines() {
  for (const auto& machine : machines_) {  // id order keeps runs deterministic
    if (machine->cpu().state() == Cpu::State::Awake) machine->cpu().run();
  }
}

void World::route_outputs(Time now) {
  for (const auto& sender : machines_) {
    for (auto& [device, frame] : sender->take_outbox()) {
      const Endpoint from{sender->id(), device};
      for (auto& [endpoint, link] : bridges_) {
        if (endpoint == from) link->send(frame);
      }
      for (Wire& wire : wires_) {
        if (!wire.attaches(from)) continue;
        std::optional<std::vector<Delivery>> deliveries = wire.transmit(from, frame, now);
        if (!deliveries) {
          trace_.record({now, from.machine, "drop", static_cast<uint32_t>(frame.size()), device});
          continue;
        }
        for (Delivery& delivery : *deliveries) {
          events_.schedule(delivery.at, [this, delivery = std::move(delivery)]() mutable {
            machine(delivery.to.machine).deliver(delivery.to.device, std::move(delivery.frame));
          });
        }
      }
    }
  }
}

void World::step_plant(Plant& plant, Time period, Time at) {
  events_.schedule(at, [this, &plant, period, at] {
    plant.step(at, *this);
    const Time next = time_add(at, period);
    if (next != kNever) step_plant(plant, period, next);
  });
}

}  // namespace costar
