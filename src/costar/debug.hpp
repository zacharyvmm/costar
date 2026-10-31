#pragma once

#include <string_view>

namespace costar {

class World;

// Debugger support (docs/DEBUGGING.md).
//
// A debugger can only find what something global points to.  As `_r_debug`
// lists loaded objects for GDB, `costar_debug_worlds` lists Worlds for
// tools/gdb/costar.py, which reads machines, CPUs and threads from them.
// A World never lists itself: a host that wants its Worlds debugged (costar
// run and test, costar-grpc's sessions) lists each one with a DebugEntry
// that it keeps while the World lives, and destroys first.  The list is
// never read by the engine.
class DebugEntry {
 public:
  // Lists `world` until destroyed.
  explicit DebugEntry(const World* world);
  ~DebugEntry();
  DebugEntry(const DebugEntry&) = delete;
  DebugEntry& operator=(const DebugEntry&) = delete;

  const World* world() const { return world_; }
  const DebugEntry* next() const { return next_; }

 private:
  const World* world_;
  DebugEntry* prev_ = nullptr;
  DebugEntry* next_ = nullptr;
};

// True if a debugger is tracing this process.
bool debugger_attached();

// Stop in the debugger, here (SIGTRAP), if one is attached, saying
// "costar: break <why>" on standard error; without one, say so and carry on.
void debug_break(std::string_view why);

}  // namespace costar

// The listed Worlds, newest first.
extern "C" costar::DebugEntry* costar_debug_worlds;
