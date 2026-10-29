#pragma once

#include <functional>
#include <optional>
#include <queue>
#include <vector>

#include "costar/time.hpp"

namespace costar {

// Host-side actions at virtual times: wire deliveries, plant steps, scenario
// injections.  Events at the same time run in the order they were scheduled.
class EventQueue {
 public:
  using Action = std::function<void()>;

  void schedule(Time at, Action action);
  std::optional<Time> next_time() const;
  // Run every event due at or before `now`, including ones they schedule.
  void run_due(Time now);

 private:
  struct Event {
    Time at;
    uint64_t sequence;
    Action action;
    bool operator>(const Event& other) const {
      return at != other.at ? at > other.at : sequence > other.sequence;
    }
  };

  std::priority_queue<Event, std::vector<Event>, std::greater<>> queue_;
  uint64_t next_sequence_ = 0;
};

}  // namespace costar
