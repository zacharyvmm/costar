#include "costar/world.hpp"

#include <algorithm>

namespace costar {

Machine& World::add_machine(const std::filesystem::path& image) {
  const auto id = static_cast<uint32_t>(machines_.size());
  machines_.push_back(std::make_unique<Machine>(id, image, now_, trace_));
  return *machines_.back();
}

void World::run_until(Time until) {
  while (const std::optional<Time> next = next_event_time()) {
    if (*next > until) break;
    now_ = *next;
    fire_events_at(now_);   // timers raise IRQs; busy CPUs finish
    run_awake_machines();   // each awake CPU runs until it halts or polls
  }
  if (until != kNever) now_ = std::max(now_, until);
}

std::optional<Time> World::next_event_time() const {
  Time next = kNever;
  for (const auto& machine : machines_) next = std::min(next, machine->cpu().next_event());
  if (next == kNever) return std::nullopt;
  return std::max(next, now_);
}

void World::fire_events_at(Time now) {
  for (const auto& machine : machines_) machine->cpu().advance_to(now);
}

void World::run_awake_machines() {
  for (const auto& machine : machines_) {  // id order keeps runs deterministic
    if (machine->cpu().state() == Cpu::State::Awake) machine->cpu().run();
  }
}

}  // namespace costar
