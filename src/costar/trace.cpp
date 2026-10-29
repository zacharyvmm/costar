#include "costar/trace.hpp"

#include <algorithm>
#include <format>

namespace costar {

std::vector<TraceRecord> Trace::for_machine(uint32_t machine) const {
  std::vector<TraceRecord> out;
  std::ranges::copy_if(records_, std::back_inserter(out),
                       [machine](const TraceRecord& r) { return r.machine == machine; });
  return out;
}

std::string format(const TraceRecord& r) {
  if (!r.text) return std::format("{} m{} {} {}", r.at, r.machine, r.label, r.value);
  if (r.text->empty()) return std::format("{} m{} {}", r.at, r.machine, r.label);
  return std::format("{} m{} {} {}", r.at, r.machine, r.label, *r.text);
}

std::string Trace::format() const {
  std::string out;
  for (const TraceRecord& r : records_) {
    out += costar::format(r);
    out += '\n';
  }
  return out;
}

}  // namespace costar
