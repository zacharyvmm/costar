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

std::string Trace::format() const {
  std::string out;
  for (const TraceRecord& r : records_) {
    if (r.text.empty()) {
      out += std::format("{} m{} {} {}\n", r.at, r.machine, r.label, r.value);
    } else {
      out += std::format("{} m{} {} {}\n", r.at, r.machine, r.label, r.text);
    }
  }
  return out;
}

}  // namespace costar
