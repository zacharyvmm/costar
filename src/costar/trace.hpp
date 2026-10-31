#pragma once

#include <cstdint>
#include <functional>
#include <optional>
#include <string>
#include <vector>

#include "costar/time.hpp"

namespace costar {

struct TraceRecord {
  Time at;
  uint32_t machine;
  std::string label;
  uint32_t value = 0;
  std::optional<std::string> text = {};  // set for text records (console output, frames, ...)

  bool operator==(const TraceRecord&) const = default;
};

// "<time ns> m<machine> <label> <value or text>"
std::string format(const TraceRecord& record);

// Everything observable that happened during a run, in order.
class Trace {
 public:
  using Sink = std::function<void(const TraceRecord&)>;

  void record(TraceRecord record) {
    records_.push_back(std::move(record));
    if (sink_) sink_(records_.back());
  }
  // Called with each record as it is recorded.
  void set_sink(Sink sink) { sink_ = std::move(sink); }
  const std::vector<TraceRecord>& records() const { return records_; }
  std::vector<TraceRecord> for_machine(uint32_t machine) const;
  // One line per record: "<time ns> m<machine> <label> <value or text>".
  std::string format() const;

 private:
  std::vector<TraceRecord> records_;
  Sink sink_;
};

}  // namespace costar
