#include "costar/cli_run.hpp"

#include <algorithm>
#include <csignal>
#include <cstdlib>
#include <future>
#include <iostream>
#include <iterator>
#include <memory>
#include <stdexcept>

#include <termios.h>
#include <unistd.h>

#include "costar/debug.hpp"

namespace costar::cli {

namespace {

// Ctrl-]: ends a console run typed on a terminal (as in telnet).
constexpr uint8_t kConsoleEscape = 0x1d;
// After standard input ends, a console run goes on until the firmware has
// been quiet this long, so the answer to the last command gets out.
constexpr Time kConsoleLinger = milliseconds(1000);

// The terminal's settings before a console put it in raw mode, for signal
// handlers and the timeout watchdog to restore; and the signal that asked a
// real-time run to end.  Process-wide, as a terminal and signals are.
termios g_saved_terminal{};
volatile sig_atomic_t g_terminal_raw = 0;
volatile sig_atomic_t g_stop_signal = 0;

extern "C" void on_stop_signal(int signal) {
  if (g_stop_signal != 0) {
    restore_terminal();
    std::signal(signal, SIG_DFL);
    std::raise(signal);
  }
  g_stop_signal = signal;
}

// While a real-time run (bridges, a console) lasts, the first SIGINT, SIGTERM
// or SIGHUP ends it as its end time would: the trace is written, bridges are
// closed (a pty's link removed) and expectations are checked.  A second one
// ends the process.
class StopSignals {
 public:
  StopSignals() {
    struct sigaction action{};
    action.sa_handler = on_stop_signal;
    sigemptyset(&action.sa_mask);
    for (size_t i = 0; i < std::size(kSignals); ++i) sigaction(kSignals[i], &action, &saved_[i]);
  }
  ~StopSignals() {
    for (size_t i = 0; i < std::size(kSignals); ++i) sigaction(kSignals[i], &saved_[i], nullptr);
    g_stop_signal = 0;
  }
  StopSignals(const StopSignals&) = delete;
  StopSignals& operator=(const StopSignals&) = delete;

  static bool raised() { return g_stop_signal != 0; }

 private:
  static constexpr int kSignals[] = {SIGINT, SIGTERM, SIGHUP};
  struct sigaction saved_[std::size(kSignals)] {};
};

// Standard input and output attached to a channel, for the length of a run.
// If standard input is a terminal it goes into raw mode: keys reach the
// firmware as typed (Ctrl-C included, as a byte), without local echo; output
// still starts a new line at '\n'.  The terminal is put back afterwards.
// SIGPIPE is ignored meanwhile: when standard output's reader goes, writes
// fail with EPIPE and the run ends (as at the end of the input), instead of
// the signal ending the process with the terminal still raw.
class ConsoleSession {
 public:
  explicit ConsoleSession(const std::string& name) : terminal_(isatty(STDIN_FILENO) == 1) {
    struct sigaction ignore{};
    ignore.sa_handler = SIG_IGN;
    sigemptyset(&ignore.sa_mask);
    sigaction(SIGPIPE, &ignore, &saved_pipe_);
    if (!terminal_ || tcgetattr(STDIN_FILENO, &g_saved_terminal) != 0) return;
    std::cerr << "costar: " << name << " is on this terminal; Ctrl-] ends the run\n";
    termios raw = g_saved_terminal;
    cfmakeraw(&raw);
    raw.c_oflag |= OPOST | ONLCR;
    g_terminal_raw = 1;
    tcsetattr(STDIN_FILENO, TCSANOW, &raw);
  }
  ~ConsoleSession() {
    restore_terminal();
    g_terminal_raw = 0;
    sigaction(SIGPIPE, &saved_pipe_, nullptr);
  }
  ConsoleSession(const ConsoleSession&) = delete;
  ConsoleSession& operator=(const ConsoleSession&) = delete;

  std::unique_ptr<StreamLink> link() const {
    return std::make_unique<StreamLink>(STDIN_FILENO, STDOUT_FILENO,
                                        terminal_ ? std::optional(kConsoleEscape) : std::nullopt,
                                        StopSignals::raised);
  }

 private:
  bool terminal_;
  struct sigaction saved_pipe_ {};
};

// The settings' sink, then a debugger break at each record --break-on matches.
Trace::Sink sink_with_breaks(World& world, const RunSettings& settings) {
  if (settings.break_on.empty()) return settings.sink;
  return [&world, &settings](const TraceRecord& r) {
    if (settings.sink) settings.sink(r);
    if (std::ranges::none_of(settings.break_on, [&](const TraceMatch& m) { return m.matches(r); })) return;
    if (settings.before_break) settings.before_break();
    const std::string why = "on " + format(r);
    if (!world.machine(r.machine).cpu().break_in_call(why)) debug_break(why);
  };
}

// Scheduled before the world is built, the break is the first of the events
// at its instant, which run in the order they were scheduled, and so comes
// before the CPUs, which come after events in a step.
void schedule_break(World& world, Time at, const std::function<void()>& before_break) {
  world.at(at, [&before_break, at] {
    if (before_break) before_break();
    debug_break("at " + std::to_string(at) + " ns");
  });
}

// Say where the host can reach bridges that listen.
void announce_bridges(const Scenario& scenario, const World& world) {
  for (const auto& [endpoint, address] : world.bridge_addresses()) {
    if (!address.empty()) {
      std::cerr << "costar: " << scenario.machines.at(endpoint.machine).name << '.' << endpoint.device
                << " is bridged at " << address << '\n';
    }
  }
}

// In real time, until `end` or a signal.
void run_until_signal(World& world, Time end) {
  const StopSignals signals;
  world.run_realtime(end, StopSignals::raised);
}

// In real time, with standard input and output on the console's channel,
// until `end`, a signal, Ctrl-], standard output closing, or kConsoleLinger
// of quiet after standard input ends.
void run_console(World& world, Time end, const Console& console) {
  const StopSignals signals;
  const ConsoleSession session(console.name);
  auto owned = session.link();
  const StreamLink& link = *owned;
  world.bridge(console.endpoint, std::move(owned));
  uint64_t written = 0;
  Time quiet_since = 0;
  world.run_realtime(end, [&] {
    if (StopSignals::raised() || link.escaped() || link.closed()) return true;  // closed: nobody reads
    if (!link.ended() || link.written() != written) {
      written = link.written();
      quiet_since = world.now();
      return false;
    }
    return world.now() - quiet_since >= kConsoleLinger;
  });
}

Run results_of(World& world) {
  Run run{world.trace(), world.stalls(), world.stack_overruns(), {}, world.now()};
  for (uint32_t id = 0; id < world.machine_count(); ++id) {
    for (ThreadStack& thread : world.machine(id).cpu().stacks()) run.stacks.emplace_back(id, std::move(thread));
  }
  return run;
}

}  // namespace

Run run_scenario(const Scenario& scenario, const PlantFactories& plants, const RunSettings& settings) {
  const Time end = settings.until.value_or(scenario.duration.value_or(kNever));
  const bool realtime = settings.realtime || !scenario.bridges.empty() || settings.console;
  if (end == kNever && !scenario.plants.empty() && !realtime) {
    throw std::runtime_error(scenario.name + ": a scenario with a plant needs a duration");
  }
  World world;
  const DebugEntry listed(&world);
  world.on_record(sink_with_breaks(world, settings));
  if (settings.break_at) schedule_break(world, *settings.break_at, settings.before_break);
  build_world(scenario, world, plants);
  announce_bridges(scenario, world);
  if (settings.console) {
    run_console(world, end, *settings.console);
  } else if (realtime) {
    run_until_signal(world, end);
  } else {
    world.run_until(end);
  }
  world.check_stacks();
  return results_of(world);
}

void restore_terminal() noexcept {
  if (g_terminal_raw) tcsetattr(STDIN_FILENO, TCSANOW, &g_saved_terminal);
}

Watchdog::Watchdog(std::optional<std::chrono::seconds> budget, std::function<void(const std::string&)> expire)
    : budget_(budget), expire_(std::move(expire)) {
  if (budget_) thread_ = std::thread([this] { watch(); });
}

Watchdog::~Watchdog() {
  {
    const std::lock_guard lock(mutex_);
    done_ = true;
  }
  wake_.notify_all();
  if (thread_.joinable()) thread_.join();
}

void Watchdog::start(const std::string& scenario) {
  if (!budget_) return;
  {
    const std::lock_guard lock(mutex_);
    scenario_ = scenario;
    deadline_ = Clock::now() + *budget_;
  }
  wake_.notify_all();
}

void Watchdog::watch() {
  std::unique_lock lock(mutex_);
  while (!done_) {
    const Clock::time_point deadline = deadline_;
    if (wake_.wait_until(lock, deadline) == std::cv_status::timeout && deadline == deadline_ && !done_) {
      auto said = std::make_shared<std::promise<void>>();
      std::future<void> done = said->get_future();
      std::thread([expire = expire_, name = scenario_, said] {
        expire(name);
        said->set_value();
      }).detach();
      done.wait_for(kGrace);
      std::_Exit(1);
    }
  }
}

}  // namespace costar::cli
