#pragma once

#include <chrono>
#include <condition_variable>
#include <cstddef>
#include <functional>
#include <future>
#include <map>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <vector>

#include "costar/scenario.hpp"
#include "costar/time.hpp"
#include "costar/trace.hpp"

namespace costar {

struct SessionStatus {
  Time now = 0;
  bool running = false;
  Time run_until = 0;
  size_t trace_length = 0;
  std::string error = {};  // why the simulation stopped, if it failed
};

// What a session's world looks like at one instant.
struct DeviceSnapshot {
  std::string name;
  DeviceKind kind;
  int irq;                                  // -1: none
  size_t queued_frames = 0;                   // Channel
  std::map<uint32_t, uint32_t> registers = {};  // Registers
  Frame memory = {};                          // Memory
};

struct MachineSnapshot {
  uint32_t id;
  std::string name;
  std::string cpu_state;  // "awake", "halted", "busy" or "off"
  std::vector<DeviceSnapshot> devices;
};

struct Snapshot {
  Time now;
  std::vector<MachineSnapshot> machines;
};

// One interactive simulation, for a remote client.
//
// A session is an actor: its own thread builds and owns the World, and every
// public method is a command that thread runs between slices of simulation.
// Nothing else ever touches the World.
//
// Inputs are recorded with the virtual time they take effect, so the history
// is deterministic: seek(t) rebuilds the world and replays to `t`.
class Session {
 public:
  // Builds the world on the session's thread.  Throws what building throws.
  Session(Scenario scenario, PlantFactories plants);
  ~Session();
  Session(const Session&) = delete;
  Session& operator=(const Session&) = delete;

  const std::string& name() const { return scenario_.name; }
  const Scenario& scenario() const { return scenario_; }

  // Run toward `until` in the background (until pause() or `until`).
  SessionStatus run(Time until);
  SessionStatus pause();
  // Apply an input at `input.at`, or now if that is in the past.
  SessionStatus input(Input input);
  // Rebuild and replay to `time`.  Recorded inputs after `time` are dropped.
  SessionStatus seek(Time time);
  SessionStatus status();
  Snapshot inspect();

  // A reader's position in the trace.  A seek rewinds the trace; readers
  // that started before it must start again.
  struct TraceCursor {
    size_t next = 0;
    std::optional<uint64_t> rewinds = std::nullopt;  // set on first use
  };

  // New trace records at the cursor, waiting up to `timeout` for some (an
  // empty result means none arrived).  Returns nullopt if a seek rewound the
  // trace since the cursor's first use, or the session is closing.
  std::optional<std::vector<TraceRecord>> wait_trace(TraceCursor& cursor, std::chrono::milliseconds timeout);

 private:
  struct State;
  using Command = std::function<void(State&)>;

  // Run `f` on the session's thread and return its result.
  template <class F>
  auto call(F f) -> decltype(f(std::declval<State&>()));

  void loop(std::promise<void> built);
  void publish(const State& state);
  SessionStatus status_of(const State& state) const;

  const Scenario scenario_;
  const PlantFactories plants_;

  std::mutex mutex_;  // guards the members below
  std::condition_variable commands_ready_;
  std::condition_variable trace_grew_;
  std::vector<Command> commands_;
  bool closing_ = false;
  std::vector<TraceRecord> published_;  // the world's trace, as of the last command or slice
  uint64_t rewinds_ = 0;

  std::thread thread_;  // last: started after everything above exists
};

}  // namespace costar
