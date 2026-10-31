// Compiled with debug information in every build (see CMakeLists.txt), so
// the types it includes (World, Machine, Cpu, threads) are described for
// tools/gdb/costar.py even when the rest of the engine has none.
#include "costar/debug.hpp"

#include <signal.h>

#include <fstream>
#include <iostream>
#include <mutex>
#include <string>

#include "costar/world.hpp"

costar::DebugEntry* costar_debug_worlds = nullptr;

namespace costar {

namespace {
std::mutex worlds_mutex;  // listed Worlds come and go on several threads (sessions)
}  // namespace

DebugEntry::DebugEntry(const World* world) : world_(world) {
  const std::lock_guard lock(worlds_mutex);
  next_ = costar_debug_worlds;
  if (next_ != nullptr) next_->prev_ = this;
  costar_debug_worlds = this;
}

DebugEntry::~DebugEntry() {
  const std::lock_guard lock(worlds_mutex);
  if (prev_ != nullptr) prev_->next_ = next_;
  if (next_ != nullptr) next_->prev_ = prev_;
  if (costar_debug_worlds == this) costar_debug_worlds = next_;
}

bool debugger_attached() {
  std::ifstream status("/proc/self/status");
  for (std::string line; std::getline(status, line);) {
    if (line.starts_with("TracerPid:")) return std::stol(line.substr(10)) != 0;
  }
  return false;
}

void debug_break(std::string_view why) {
  if (!debugger_attached()) {
    std::cerr << "costar: break " << why << " (no debugger attached: carrying on)\n";
    return;
  }
  std::cerr << "costar: break " << why << '\n';
  raise(SIGTRAP);  // `up` from here reaches the code that got here (firmware, for a trace record)
}

}  // namespace costar
