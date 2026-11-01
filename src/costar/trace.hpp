#pragma once

#include <cstdint>
#include <functional>
#include <optional>
#include <string>
#include <string_view>
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

// Which records to keep (`costar run --machine/--label`, `[expect]`'s
// trace_machines and trace_labels): those from one of `machines` (any if
// none), with one of `labels` (any if none), and with none of
// `excluded_labels`.  A label ending in '*' matches every label that starts
// with what comes before it.
struct TraceFilter {
  std::vector<uint32_t> machines = {};
  std::vector<std::string> labels = {};
  std::vector<std::string> excluded_labels = {};

  bool empty() const { return machines.empty() && labels.empty() && excluded_labels.empty(); }
  bool keeps(const TraceRecord& record) const;
};

// One JSON object (JSON lines), for tools:
//   {"time_ns":5000000,"machine":"host","label":"tx","value":2,"device":"uart","hex":"6869"}
// The record's text comes apart into "device" and "hex" (tx, rx), "device"
// (drop), "device", "offset" and "hex" (mem), or "device" and "register"
// (reg, set); any other text (log) is "text".
std::string format_json(const TraceRecord& record, std::string_view machine);

// The trace in Chrome's trace-event format (a JSON array), which
// ui.perfetto.dev and chrome://tracing open: a track per machine with an
// instant event per record (its fields, as in format_json(), as args), and a
// counter track per register written (reg, set).  Written as the run goes:
// begin(), then event() for each record, then end().  A file cut short (a
// timeout) still opens: the closing bracket is optional in this format.
class PerfettoTrace {
 public:
  PerfettoTrace(std::string title, std::vector<std::string> machines);
  std::string begin() const;
  std::string event(const TraceRecord& record) const;
  static std::string end() { return "\n]\n"; }

 private:
  std::string title_;
  std::vector<std::string> machines_;
};

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
  std::string format(const TraceFilter& filter = {}) const;

 private:
  std::vector<TraceRecord> records_;
  Sink sink_;
};

}  // namespace costar
