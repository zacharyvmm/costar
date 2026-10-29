#include "costar/events.hpp"

namespace costar {

void EventQueue::schedule(Time at, Action action) {
  queue_.push(Event{at, next_sequence_++, std::move(action)});
}

std::optional<Time> EventQueue::next_time() const {
  if (queue_.empty()) return std::nullopt;
  return queue_.top().at;
}

void EventQueue::run_due(Time now) {
  while (!queue_.empty() && queue_.top().at <= now) {
    Action action = std::move(const_cast<Event&>(queue_.top()).action);
    queue_.pop();
    action();
  }
}

}  // namespace costar
