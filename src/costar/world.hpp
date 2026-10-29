#pragma once

#include <filesystem>
#include <memory>
#include <optional>
#include <vector>

#include "costar/devices.hpp"
#include "costar/events.hpp"
#include "costar/machine.hpp"
#include "costar/plant.hpp"
#include "costar/time.hpp"
#include "costar/trace.hpp"
#include "costar/wires.hpp"

namespace costar {

// Machines, the wires between them, and the plant around them, sharing one
// virtual clock.
class World {
 public:
  World() = default;
  World(const World&) = delete;
  World& operator=(const World&) = delete;

  // ── Building ────────────────────────────────────────────────────────
  Machine& add_machine(const std::filesystem::path& image, const std::vector<DeviceSpec>& devices = {});
  // A machine with devices but no firmware: an endpoint the host drives.
  Machine& add_passive_machine(const std::vector<DeviceSpec>& devices);
  void connect(Wire wire);
  // Step `plant` now and then every `period`.
  void add_plant(std::unique_ptr<Plant> plant, Time period);
  // Run a host action at `at` (for example, inject a frame or set a sensor).
  void at(Time when, EventQueue::Action action);

  // ── Running ─────────────────────────────────────────────────────────
  // Run until `until`, or until nothing is left to happen.  Time then reads
  // `until` (if finite), even if the last event was earlier.
  void run_until(Time until);
  void run() { run_until(kNever); }
  // Run every event up to `until`, leaving time at the last one.
  void run_events_until(Time until);
  // Steps taken so far.  A step handles everything due at one instant (host
  // events, then CPUs, then routing); the sequence of steps is the same
  // however a run is sliced, so a step count marks an exact point in it.
  uint64_t steps() const { return steps_; }
  // Take steps until `count` have been taken.
  void run_steps(uint64_t count);
  // When the next event is due (nullopt: nothing will ever happen).
  std::optional<Time> next_event_time() const;

  Time now() const { return now_; }
  // True when nothing is left to happen: no events, no timers, no awake CPU.
  bool quiescent() const { return !next_event_time().has_value(); }
  size_t machine_count() const { return machines_.size(); }
  Machine& machine(uint32_t id) { return *machines_.at(id); }
  const Trace& trace() const { return trace_; }

 private:
  void fire_events_at(Time now);
  void run_awake_machines();
  void route_outputs(Time now);
  void step_plant(Plant& plant, Time period, Time at);

  void step(Time at);

  Time now_ = 0;
  uint64_t steps_ = 0;
  Trace trace_;
  EventQueue events_;
  std::vector<Wire> wires_;
  std::vector<std::unique_ptr<Plant>> plants_;
  std::vector<std::unique_ptr<Machine>> machines_;  // Last: destroyed first.
};

}  // namespace costar
