#pragma once

#include <filesystem>
#include <memory>
#include <optional>
#include <vector>

#include "costar/machine.hpp"
#include "costar/time.hpp"
#include "costar/trace.hpp"

namespace costar {

// Machines sharing one virtual clock.
class World {
 public:
  World() = default;
  World(const World&) = delete;
  World& operator=(const World&) = delete;

  Machine& add_machine(const std::filesystem::path& image);

  // Run until `until`, or until nothing is left to happen.
  void run_until(Time until);
  void run() { run_until(kNever); }

  Time now() const { return now_; }
  const Trace& trace() const { return trace_; }

 private:
  std::optional<Time> next_event_time() const;
  void fire_events_at(Time now);
  void run_awake_machines();

  Time now_ = 0;
  Trace trace_;
  std::vector<std::unique_ptr<Machine>> machines_;
};

}  // namespace costar
