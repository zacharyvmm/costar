#pragma once

#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <functional>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include "costar/scenario.hpp"
#include "costar/session.hpp"
#include "costar/world.hpp"

// How the `costar` command runs one scenario in this process.  Internal to
// cli.cpp.
namespace costar::cli {

// Standard input and output on a channel (`costar run --console`).
struct Console {
  std::string name;  // "machine.device", as given
  Endpoint endpoint;
};

struct RunSettings {
  std::optional<Time> until = {};       // none: the scenario's duration
  bool realtime = false;                // scenarios with host bridges, and console runs, always are
  std::optional<Console> console = {};
  Trace::Sink sink = {};                // each trace record, as it is recorded
  // Debugger breaks (costar run --break-at-ms, --break-on): at a virtual
  // time, before anything due then; and at trace records, inside the call
  // that records them (the firmware's, for its own records: back on the
  // firmware thread's stack, once the call's host work is done), so the
  // debugger stops with the firmware's frames on the stack.
  std::optional<Time> break_at = {};
  std::vector<TraceMatch> break_on = {};
  std::function<void()> before_break = {};  // write out what there is first
};

// What a run leaves to check.
struct Run {
  Trace trace;
  std::vector<Stall> stalls;
  std::vector<StackOverrun> overruns;
  std::vector<std::pair<uint32_t, ThreadStack>> stacks = {};  // each machine's live threads at the end
  Time end = 0;                                                // virtual time when the run ended
};

// Build the scenario's world and run it to the end.  A real-time run ends
// early at a signal (SIGINT, SIGTERM or SIGHUP; a second one ends the
// process).  A console run also ends at Ctrl-], or once its input has ended
// and the firmware has been quiet for a while.  At the end, every thread's
// whole stack is scanned for overruns its canary missed.  The world is
// listed for debuggers (DebugEntry).
Run run_scenario(const Scenario& scenario, const PlantFactories& plants, const RunSettings& settings);

// Put back a terminal that a console run put in raw mode.  Async-signal-safe.
void restore_terminal() noexcept;

// Ends the process when a scenario runs longer than its wall-clock budget.
// Firmware in a busy loop that makes no costar calls never gives control
// back, so only another thread can notice.  `expire` runs with the
// scenario's name just before the process exits with status 1.  It runs on a
// thread of its own and gets at most kGrace: its output may block (a pipe or
// terminal nobody reads, or the main thread blocked writing to one while
// holding the output's lock), and that must not keep a timed-out run alive.
class Watchdog {
 public:
  using Clock = std::chrono::steady_clock;

  Watchdog(std::optional<std::chrono::seconds> budget, std::function<void(const std::string&)> expire);
  ~Watchdog();
  Watchdog(const Watchdog&) = delete;
  Watchdog& operator=(const Watchdog&) = delete;

  // A scenario starts: its budget starts now.
  void start(const std::string& scenario);

 private:
  void watch();

  static constexpr std::chrono::seconds kGrace{2};

  std::optional<std::chrono::seconds> budget_;
  std::function<void(const std::string&)> expire_;
  std::mutex mutex_;
  std::condition_variable wake_;
  bool done_ = false;
  std::string scenario_;
  Clock::time_point deadline_ = Clock::time_point::max();
  std::thread thread_;
};

}  // namespace costar::cli
