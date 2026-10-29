#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "costar/time.hpp"

namespace costar {

struct TraceRecord {
  Time at;
  uint32_t machine;
  std::string label;
  uint32_t value = 0;
  std::string text = {};  // set for text output (costar_log)

  bool operator==(const TraceRecord&) const = default;
};

// Everything observable that happened during a run, in order.
class Trace {
 public:
  void record(TraceRecord record) { records_.push_back(std::move(record)); }
  const std::vector<TraceRecord>& records() const { return records_; }
  std::vector<TraceRecord> for_machine(uint32_t machine) const;
  // One line per record: "<time ns> m<machine> <label> <value or text>".
  std::string format() const;

 private:
  std::vector<TraceRecord> records_;
};

}  // namespace costar
