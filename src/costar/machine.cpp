#include "costar/machine.hpp"

#include "costar/abi.hpp"

namespace costar {

namespace {
void run_entry(void* entry) { reinterpret_cast<int (*)()>(entry)(); }
}  // namespace

Machine::Machine(uint32_t id, const std::filesystem::path& image, const Time& now, Trace& trace)
    : id_(id), now_(now), trace_(trace), image_(image), cpu_(now) {
  image_.attach(host_table(), this);
  cpu_.boot(run_entry, reinterpret_cast<void*>(image_.entry()));
}

void Machine::trace(const char* label, uint32_t value) { trace_.record({now_, id_, label, value}); }

}  // namespace costar
