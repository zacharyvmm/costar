#include "costar/cli.hpp"

#include <algorithm>
#include <cerrno>
#include <cctype>
#include <charconv>
#include <chrono>
#include <condition_variable>
#include <csignal>
#include <cstdlib>
#include <format>
#include <fstream>
#include <functional>
#include <future>
#include <iterator>
#include <iostream>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <sstream>
#include <stdexcept>
#include <string>
#include <string_view>
#include <thread>
#include <vector>

#include <sys/stat.h>
#include <termios.h>
#include <unistd.h>

#include "costar/debug.hpp"
#include "costar/diff.hpp"
#include "costar/session.hpp"
#include "costar/world.hpp"

namespace costar::cli {

namespace fs = std::filesystem;

namespace {

constexpr const char* kUsage =
    "usage:\n"
    "  costar run  <scenario.toml> [--images DIR]... [--set KEY=VALUE]... [--until-ms N] [--trace FILE]\n"
    "              [--realtime] [--timeout-s N] [--stacks]\n"
    "              [--console MACHINE.DEVICE] [--format text|jsonl|console] [--perfetto FILE]\n"
    "              [--machine NAME]... [--label LABEL]... [--exclude-label LABEL]...\n"
    "              [--break-at-ms N] [--break-on [MACHINE:]LABEL[=VALUE]]...\n"
    "  costar test <scenario.toml | DIR>... [--images DIR]... [--set KEY=VALUE]... [--update] [--timeout-s N]\n"
    "              [--artifacts DIR [--all-artifacts]] [--junit FILE]\n";

// A problem with the command line itself.
struct UsageError : std::runtime_error {
  using std::runtime_error::runtime_error;
};

// How `run` writes the trace (--format): costar's text, JSON lines, or only
// the console text (`log` records) as a console shows it.
enum class TraceFormat { Text, JsonLines, Console };

struct Options {
  std::string program;  // argv[0], for the commands that reproduce a failure
  std::string command;
  std::vector<fs::path> paths = {};
  std::vector<fs::path> image_dirs = {};
  std::vector<Override> overrides = {};  // --set, in order
  std::optional<Time> until = {};
  std::optional<fs::path> trace_file = {};
  bool update = false;
  bool realtime = false;
  bool stacks = false;  // report every thread's stack use at the end
  std::optional<std::chrono::seconds> timeout = {};  // wall-clock budget per scenario
  std::optional<std::string> console = {};           // "machine.device" on standard input and output
  TraceFormat format = TraceFormat::Text;            // --format
  std::optional<fs::path> perfetto_file = {};
  std::vector<std::string> machines = {};  // the trace filter (TraceFilter), by name
  std::vector<std::string> labels = {};
  std::vector<std::string> excluded_labels = {};
  std::optional<fs::path> artifacts_dir = {};  // test: what failing scenarios leave behind
  bool all_artifacts = false;                  // ... and passing ones
  std::optional<fs::path> junit_file = {};
  std::optional<Time> break_at = {};                 // stop in the debugger at this virtual time
  std::vector<std::string> break_on = {};            // ... and at these trace records
};

// A whole number between `min` and `max`, for `option`.
uint64_t parse_whole(const std::string& text, const std::string& option, uint64_t min, uint64_t max,
                     const char* unit) {
  uint64_t n = 0;
  const auto [end, error] = std::from_chars(text.data(), text.data() + text.size(), n);
  if (error != std::errc() || end != text.data() + text.size() || n < min || n > max) {
    throw UsageError(option + " needs a whole number of " + unit + ", not '" + text + "'");
  }
  return n;
}

Options parse_args(int argc, char** argv) {
  if (argc < 2) throw UsageError("missing command");
  Options options{.program = argv[0], .command = argv[1]};
  for (int i = 2; i < argc; ++i) {
    const std::string arg = argv[i];
    auto value = [&]() -> std::string {
      if (++i >= argc) throw UsageError(arg + " needs a value");
      return argv[i];
    };
    if (arg == "--images") {
      options.image_dirs.emplace_back(value());
    } else if (arg == "--set") {
      try {
        options.overrides.push_back(parse_override(value()));
      } catch (const std::invalid_argument& error) {
        throw UsageError(error.what());
      }
    } else if (arg == "--until-ms") {
      options.until = milliseconds(parse_whole(value(), arg, 0, kNever / 1'000'000, "milliseconds"));
    } else if (arg == "--trace") {
      options.trace_file = value();
    } else if (arg == "--update") {
      options.update = true;
    } else if (arg == "--realtime") {
      options.realtime = true;
    } else if (arg == "--stacks") {
      options.stacks = true;
    } else if (arg == "--console") {
      options.console = value();
    } else if (arg == "--format") {
      const std::string format = value();
      if (format == "text") {
        options.format = TraceFormat::Text;
      } else if (format == "jsonl") {
        options.format = TraceFormat::JsonLines;
      } else if (format == "console") {
        options.format = TraceFormat::Console;
      } else {
        throw UsageError("--format is text, jsonl or console, not '" + format + "'");
      }
    } else if (arg == "--perfetto") {
      options.perfetto_file = value();
    } else if (arg == "--machine") {
      options.machines.push_back(value());
    } else if (arg == "--label") {
      options.labels.push_back(value());
    } else if (arg == "--exclude-label") {
      options.excluded_labels.push_back(value());
    } else if (arg == "--artifacts") {
      options.artifacts_dir = value();
    } else if (arg == "--all-artifacts") {
      options.all_artifacts = true;
    } else if (arg == "--junit") {
      options.junit_file = value();
    } else if (arg == "--timeout-s") {
      options.timeout = std::chrono::seconds(parse_whole(value(), arg, 1, 1'000'000'000, "seconds (at least 1)"));
    } else if (arg == "--break-at-ms") {
      options.break_at = milliseconds(parse_whole(value(), arg, 0, kNever / 1'000'000 - 1, "milliseconds"));
    } else if (arg == "--break-on") {
      options.break_on.push_back(value());
    } else if (arg.starts_with("--")) {
      throw UsageError("unknown option " + arg);
    } else {
      options.paths.emplace_back(arg);
    }
  }
  if (options.paths.empty()) throw UsageError("no scenario given");
  if (options.command != "run") {
    for (const auto& [given, option] : {std::pair{options.console.has_value(), "--console"},
                                        {options.format != TraceFormat::Text, "--format"},
                                        {options.perfetto_file.has_value(), "--perfetto"},
                                        {!options.machines.empty(), "--machine"},
                                        {!options.labels.empty(), "--label"},
                                        {!options.excluded_labels.empty(), "--exclude-label"},
                                        {options.break_at.has_value(), "--break-at-ms"},
                                        {!options.break_on.empty(), "--break-on"}}) {
      if (given) throw UsageError(std::string(option) + " goes with run");
    }
  }
  if (options.command != "test") {
    for (const auto& [given, option] : {std::pair{options.artifacts_dir.has_value(), "--artifacts"},
                                        {options.all_artifacts, "--all-artifacts"},
                                        {options.junit_file.has_value(), "--junit"}}) {
      if (given) throw UsageError(std::string(option) + " goes with test");
    }
  }
  if (options.all_artifacts && !options.artifacts_dir) throw UsageError("--all-artifacts needs --artifacts DIR");
  return options;
}

// "[MACHINE:]LABEL[=VALUE]": trace records to break at.
TraceMatch parse_break_on(const std::string& text, const Scenario& scenario) {
  TraceMatch match;
  std::string rest = text;
  if (const size_t colon = rest.find(':'); colon != std::string::npos) {
    match.machine = scenario.machine_id(rest.substr(0, colon));
    rest.erase(0, colon + 1);
  }
  if (const size_t equals = rest.find('='); equals != std::string::npos) {
    match.value = static_cast<uint32_t>(parse_whole(rest.substr(equals + 1), "--break-on " + text, 0, UINT32_MAX,
                                                    "(a record's value)"));
    rest.erase(equals);
  }
  if (rest.empty()) throw UsageError("--break-on " + text + " needs a label");
  match.label = rest;
  return match;
}

// Scenario files named on the command line; directories contribute their
// *.toml files in name order.
std::vector<fs::path> scenario_files(const std::vector<fs::path>& paths) {
  std::vector<fs::path> files;
  for (const fs::path& path : paths) {
    if (!fs::is_directory(path)) {
      files.push_back(path);  // load_scenario() rejects anything but a readable file
      continue;
    }
    std::vector<fs::path> found;
    for (const auto& entry : fs::directory_iterator(path)) {
      if (entry.is_regular_file() && entry.path().extension() == ".toml") found.push_back(entry.path());
    }
    std::ranges::sort(found);
    files.insert(files.end(), found.begin(), found.end());
  }
  return files;
}

// ── Real-time runs and consoles ───────────────────────────────────────

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

void restore_terminal() noexcept {
  if (g_terminal_raw) tcsetattr(STDIN_FILENO, TCSANOW, &g_saved_terminal);
}

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

// "machine.device", a channel of the scenario's.
Endpoint console_endpoint(const Scenario& scenario, const std::string& text) {
  const size_t dot = text.find('.');
  if (dot == std::string::npos) throw UsageError("--console needs machine.device, not '" + text + "'");
  const std::string machine = text.substr(0, dot);
  const std::string device = text.substr(dot + 1);
  for (uint32_t id = 0; id < scenario.machines.size(); ++id) {
    if (scenario.machines[id].name != machine) continue;
    for (const DeviceSpec& spec : scenario.machines[id].devices) {
      if (spec.name == device && spec.kind == DeviceKind::Channel) return {id, device};
    }
    throw UsageError("--console: machine '" + machine + "' has no channel '" + device + "'");
  }
  throw UsageError("--console: no machine named '" + machine + "'");
}

// ── Running ───────────────────────────────────────────────────────────

struct Run {
  Trace trace;
  std::vector<Stall> stalls;
  std::vector<StackOverrun> overruns;
  std::vector<std::pair<uint32_t, ThreadStack>> stacks = {};  // each machine's live threads at the end
  Time end = 0;                                                // virtual time when the run ended
};

// Scenarios with host bridges always run in real time, and so do console
// runs.  A real-time run ends early at a signal (StopSignals).  A console run
// also ends at Ctrl-], or once its input has ended and the firmware has been
// quiet for kConsoleLinger.  At the end, every thread's whole stack is
// scanned for overruns its canary missed.  The world is listed for debuggers
// (DebugEntry).  `setup` gets the world before it is built, so events it
// schedules come before the scenario's at the same instant.
Run run_scenario(const Scenario& scenario, const PlantFactories& plants, std::optional<Time> until, bool realtime,
                 Trace::Sink sink = {}, const std::optional<std::string>& console = {},
                 const std::function<void(World&)>& setup = {}) {
  const Time end = until.value_or(scenario.duration.value_or(kNever));
  realtime = realtime || !scenario.bridges.empty() || console;
  if (end == kNever && !scenario.plants.empty() && !realtime) {
    throw std::runtime_error(scenario.name + ": a scenario with a plant needs a duration");
  }
  World world;
  const DebugEntry listed(&world);
  world.on_record(std::move(sink));
  if (setup) setup(world);
  build_world(scenario, world, plants);
  for (const auto& [endpoint, address] : world.bridge_addresses()) {
    if (!address.empty()) {
      std::cerr << "costar: " << scenario.machines.at(endpoint.machine).name << '.' << endpoint.device
                << " is bridged at " << address << '\n';
    }
  }
  if (console) {
    const Endpoint endpoint = console_endpoint(scenario, *console);
    const StopSignals signals;
    const ConsoleSession session(*console);
    auto owned = session.link();
    const StreamLink& link = *owned;
    world.bridge(endpoint, std::move(owned));
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
  } else if (realtime) {
    const StopSignals signals;
    world.run_realtime(end, StopSignals::raised);
  } else {
    world.run_until(end);
  }
  world.check_stacks();
  Run run{world.trace(), world.stalls(), world.stack_overruns(), {}, world.now()};
  for (uint32_t id = 0; id < world.machine_count(); ++id) {
    for (ThreadStack& thread : world.machine(id).cpu().stacks()) run.stacks.emplace_back(id, std::move(thread));
  }
  return run;
}

std::string read_file(const fs::path& file) {
  std::ifstream in(file, std::ios::binary);
  std::string text;
  if (in && fs::is_regular_file(file)) text.assign(std::istreambuf_iterator<char>(in), {});
  if (!in || in.bad() || !fs::is_regular_file(file)) throw std::runtime_error("cannot read " + file.string());
  return text;
}

// Write all of `text` to `fd`, from any thread, without the C++ streams.
void say(int fd, std::string_view text) noexcept {
  for (size_t done = 0; done < text.size();) {
    const ssize_t n = write(fd, text.data() + done, text.size() - done);
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) return;
    done += static_cast<size_t>(n);
  }
}

// Text for standard output, or for a file.  A file is written through a new
// sibling file of our own (never an existing one), renamed into place by
// commit(): a failed write leaves the old file as it was.  Text is written as
// it comes, in blocks, so the timeout watchdog's thread can commit what a
// hung run has written so far.
class Output {
 public:
  explicit Output(std::optional<fs::path> file) : file_(std::move(file)) {
    if (!file_) return;
    temporary_ = file_->string() + ".XXXXXX";
    fd_ = mkstemp(temporary_.data());
    if (fd_ < 0) throw std::runtime_error("cannot write " + file_->string());
    const mode_t mask = umask(0);
    umask(mask);
    ok_ = fchmod(fd_, 0666 & ~mask) == 0;
  }
  ~Output() {
    if (!file_ || committed_) return;
    close(fd_);
    std::error_code error;
    fs::remove(temporary_, error);
  }
  Output(const Output&) = delete;
  Output& operator=(const Output&) = delete;

  void append(std::string_view text) {
    const std::lock_guard lock(mutex_);
    buffer_ += text;
    if (buffer_.size() >= kBlock) flush();
  }

  // Write out what has been appended so far.
  void sync() {
    const std::lock_guard lock(mutex_);
    flush();
  }

  // Write out the rest; a file is renamed into place.
  void commit() {
    if (try_commit()) return;
    throw std::runtime_error(file_ ? "cannot write " + file_->string() : "cannot write to standard output");
  }
  bool try_commit() noexcept {
    const std::lock_guard lock(mutex_);
    if (committed_) return ok_;
    committed_ = true;
    flush();
    return finish();
  }

 private:
  static constexpr size_t kBlock = 64 * 1024;

  // A file is closed and renamed into place.
  bool finish() noexcept {
    if (!file_) return ok_;
    ok_ = close(fd_) == 0 && ok_;
    std::error_code error;
    if (ok_) fs::rename(temporary_, *file_, error);
    if (!ok_ || error) {
      fs::remove(temporary_, error);
      ok_ = false;
    }
    return ok_;
  }

  void flush() noexcept {
    for (size_t done = 0; ok_ && done < buffer_.size();) {
      const ssize_t n = write(fd_, buffer_.data() + done, buffer_.size() - done);
      if (n < 0 && errno == EINTR) continue;
      ok_ = n > 0;
      done += ok_ ? static_cast<size_t>(n) : 0;
    }
    buffer_.clear();
  }

  std::optional<fs::path> file_;
  std::string temporary_;
  int fd_ = STDOUT_FILENO;
  bool ok_ = true;
  bool committed_ = false;
  std::mutex mutex_;
  std::string buffer_;
};

void write_file(const fs::path& file, const std::string& content) {
  Output out(file);
  out.append(content);
  out.commit();
}

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

  Watchdog(std::optional<std::chrono::seconds> budget, std::function<void(const std::string&)> expire)
      : budget_(budget), expire_(std::move(expire)) {
    if (budget_) thread_ = std::thread([this] { watch(); });
  }
  ~Watchdog() {
    {
      const std::lock_guard lock(mutex_);
      done_ = true;
    }
    wake_.notify_all();
    if (thread_.joinable()) thread_.join();
  }
  Watchdog(const Watchdog&) = delete;
  Watchdog& operator=(const Watchdog&) = delete;

  // A scenario starts: its budget starts now.
  void start(const std::string& scenario) {
    if (!budget_) return;
    {
      const std::lock_guard lock(mutex_);
      scenario_ = scenario;
      deadline_ = Clock::now() + *budget_;
    }
    wake_.notify_all();
  }

 private:
  void watch() {
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

std::string timeout_message(std::chrono::seconds budget) {
  return "timed out after " + std::to_string(budget.count()) +
         " s of wall-clock time (firmware in a busy loop that makes no costar calls? build it with INSTRUMENT, "
         "or call costar_cpu_poll() in the loop)";
}

// One message per machine whose firmware stalled.
std::vector<std::string> stall_failures(const Scenario& scenario, const std::vector<Stall>& stalls) {
  std::vector<std::string> out;
  for (const Stall& s : stalls) {
    out.push_back("machine " + scenario.machines.at(s.machine).name + " stalled at " + std::to_string(s.at) +
                  " ns: its firmware made more than " + std::to_string(scenario.stall_limit.value_or(kDefaultStallLimit)) +
                  " costar calls without letting time pass (a busy loop? build it with INSTRUMENT, or call "
                  "costar_cpu_poll() in the loop; or raise stall_limit)");
  }
  return out;
}

// One message per thread that overran its stack.
std::vector<std::string> stack_failures(const Scenario& scenario, const std::vector<StackOverrun>& overruns) {
  std::vector<std::string> out;
  for (const StackOverrun& o : overruns) {
    out.push_back("machine " + scenario.machines.at(o.machine).name + ": thread '" + o.thread.name +
                  "' overran its stack at " + std::to_string(o.at) + " ns: it used " +
                  std::to_string(o.thread.used) + " bytes, more than the " + std::to_string(o.thread.limit) +
                  " allowed for its " + std::to_string(o.thread.declared) +
                  "-byte stack (give it a bigger stack, or raise stack_check's allowance if only host frames are "
                  "bigger)");
  }
  return out;
}

// "machine m, thread t: 1840 bytes of stack used, 12288 allowed (2048 declared)"
std::string stack_report(const Scenario& scenario, uint32_t machine, const ThreadStack& thread) {
  std::string out = "machine " + scenario.machines.at(machine).name + ", thread '" + thread.name +
                    "': " + std::to_string(thread.used) + " bytes of stack used";
  if (thread.limit != 0) out += ", " + std::to_string(thread.limit) + " allowed";
  out += thread.declared != 0 ? " (" + std::to_string(thread.declared) + " declared)" : " (size unknown)";
  return out;
}

std::vector<std::string> machine_names(const Scenario& scenario) {
  std::vector<std::string> names;
  for (const MachineSpec& machine : scenario.machines) names.push_back(machine.name);
  return names;
}

// --machine, --label and --exclude-label, with machines named in the scenario.
TraceFilter trace_filter(const Scenario& scenario, const Options& options) {
  TraceFilter filter{.labels = options.labels, .excluded_labels = options.excluded_labels};
  for (const std::string& name : options.machines) {
    const auto& machines = scenario.machines;
    const auto found = std::ranges::find(machines, name, &MachineSpec::name);
    if (found == machines.end()) throw UsageError("--machine: no machine named '" + name + "'");
    filter.machines.push_back(static_cast<uint32_t>(found - machines.begin()));
  }
  return filter;
}

// Where `run` writes the trace as it goes, filtered: text, JSON lines or
// console text on standard output or in a --trace file (not on standard
// output if a console has it), and a Perfetto file.  Console text is each
// `log` record's text, after "<machine>: " if the filter keeps several
// machines, written out line by line.
class TraceOutputs {
 public:
  TraceOutputs(const Options& options, const Scenario& scenario)
      : filter_(trace_filter(scenario, options)),
        names_(machine_names(scenario)),
        format_(options.format),
        several_((filter_.machines.empty() ? names_.size() : filter_.machines.size()) > 1),
        perfetto_(scenario.name, names_) {
    if (!options.console || options.trace_file) trace_.emplace(options.trace_file);
    if (options.perfetto_file) {
      perfetto_file_.emplace(*options.perfetto_file);
      perfetto_file_->append(perfetto_.begin());
    }
  }

  void record(const TraceRecord& r) {
    if (!filter_.keeps(r)) return;
    if (trace_) {
      switch (format_) {
        case TraceFormat::Text: trace_->append(format(r) + '\n'); break;
        case TraceFormat::JsonLines: trace_->append(format_json(r, names_.at(r.machine)) + '\n'); break;
        case TraceFormat::Console:
          if (r.label != "log" || !r.text) break;
          trace_->append((several_ ? names_.at(r.machine) + ": " : "") + *r.text + '\n');
          trace_->sync();
          break;
      }
    }
    if (perfetto_file_) perfetto_file_->append(perfetto_.event(r));
  }

  // Everything is written, and files are renamed into place.
  void commit() {
    if (perfetto_file_) perfetto_file_->append(PerfettoTrace::end());
    if (trace_) trace_->commit();
    if (perfetto_file_) perfetto_file_->commit();
  }
  // What there is so far (a failed or timed-out run), from any thread.
  void try_commit() noexcept {
    if (trace_) trace_->try_commit();
    if (perfetto_file_) perfetto_file_->try_commit();
  }
  // Write out what has been recorded so far (before a debugger break).
  void sync() {
    if (trace_) trace_->sync();
    if (perfetto_file_) perfetto_file_->sync();
  }

 private:
  TraceFilter filter_;
  std::vector<std::string> names_;
  TraceFormat format_;
  bool several_;  // machines whose console text is shown
  PerfettoTrace perfetto_;
  std::optional<Output> trace_;
  std::optional<Output> perfetto_file_;
};

// The trace goes out as the run goes.  A run that fails or times out still
// writes the trace so far.  Stalls and failed expectations are reported on
// standard error and make the command fail.  Expectations are checked
// against the whole trace, whatever the filter.
int run_command(const Options& options, const PlantFactories& plants) {
  const Scenario scenario = load_scenario(options.paths.front(), options.image_dirs, options.overrides);
  TraceOutputs outputs(options, scenario);
  Watchdog watchdog(options.timeout, [&](const std::string& name) {
    restore_terminal();
    // The message first: committing the output may block.
    say(STDERR_FILENO, "costar: " + name + ": " + timeout_message(*options.timeout) + "\n");
    outputs.try_commit();
  });
  watchdog.start(scenario.name);
  // Breaks for a debugger: at a trace record, inside the call that records
  // it (the firmware's, for its own records: back on the firmware thread's
  // stack, once the call's host work is done), so the debugger stops with
  // its frames on the stack; and at a virtual time, before anything due then.
  std::vector<TraceMatch> break_on;
  for (const std::string& spec : options.break_on) break_on.push_back(parse_break_on(spec, scenario));
  World* running = nullptr;  // set once the world exists
  const auto sink = [&](const TraceRecord& r) {
    outputs.record(r);
    if (std::ranges::any_of(break_on, [&](const TraceMatch& m) { return m.matches(r); })) {
      outputs.sync();
      const std::string why = "on " + format(r);
      if (running == nullptr || !running->machine(r.machine).cpu().break_in_call(why)) debug_break(why);
    }
  };
  const auto setup = [&](World& world) {
    running = &world;
    if (!options.break_at) return;
    // Scheduled before the world is built: first of the events at its
    // instant, which run in the order they were scheduled, and so before
    // the CPUs, which come after events in a step.
    world.at(*options.break_at, [&outputs, at = *options.break_at] {
      outputs.sync();
      debug_break("at " + std::to_string(at) + " ns");
    });
  };
  Run run;
  try {
    run = run_scenario(scenario, plants, options.until, options.realtime, sink, options.console, setup);
  } catch (...) {
    outputs.try_commit();
    throw;
  }
  outputs.commit();
  if (options.stacks) {
    for (const auto& [machine, thread] : run.stacks) {
      std::cerr << "costar: " << stack_report(scenario, machine, thread) << '\n';
    }
  }
  std::vector<std::string> failed = stall_failures(scenario, run.stalls);
  std::ranges::move(stack_failures(scenario, run.overruns), std::back_inserter(failed));
  std::ranges::move(check_expectations(scenario, run.trace), std::back_inserter(failed));
  for (const std::string& failure : failed) std::cerr << "costar: " << failure << '\n';
  return failed.empty() ? 0 : 1;
}

// ── costar test ───────────────────────────────────────────────────────

// Hunks of a golden trace's diff that `test` prints, and lines per hunk.
constexpr DiffOptions kGoldenDiff{.max_hunks = 5, .max_hunk_lines = 40};

// `text`, each line indented by `indent`.
std::string indented(std::string_view text, std::string_view indent) {
  std::string out;
  while (!text.empty()) {
    const size_t end = std::min(text.find('\n'), text.size());
    out += indent;
    out += text.substr(0, end);
    out += '\n';
    text.remove_prefix(std::min(end + 1, text.size()));
  }
  return out;
}

// A scenario's trace against its golden file.
struct GoldenCheck {
  std::string problem;    // "" if it matches; else the first hunks of a unified diff
  std::string headline;   // the problem, up to the end of its first hunk
  std::string full_diff;  // every hunk, when asked for
};

GoldenCheck check_golden(const Scenario& scenario, const std::string& actual, bool full_diff) {
  GoldenCheck check;
  std::string expected;
  try {
    expected = read_file(*scenario.expected_trace);
  } catch (const std::exception& error) {
    check.problem = check.headline = error.what();
    return check;
  }
  const Diff diff = diff_lines(expected, actual, kGoldenDiff);
  if (diff.empty()) return check;
  const std::string header = scenario.expected_trace->string() + ": the trace differs (- golden, + actual):\n";
  check.problem = header + indented(diff.text(), "    ");
  check.problem.pop_back();  // the caller ends the line
  check.headline = header + (diff.hunks.empty() ? diff.text() : diff.hunks.front());
  check.headline.pop_back();
  if (full_diff) check.full_diff = diff_lines(expected, actual).text();
  return check;
}

// A scenario's outcome, for the JUnit report.
struct Result {
  enum class Status { Pass, Fail, Skip };
  std::string name;
  fs::path file;
  Status status;
  std::vector<std::string> problems = {};  // Fail: why; Skip: why
  std::string headline = {};               // Fail: the first problem, shortly
  double seconds = 0;
};

// One <testsuite> for the run, a <testcase> per scenario.
std::string junit_xml(const std::vector<Result>& results, double seconds) {
  const auto count = [&](Result::Status status) {
    return std::ranges::count(results, status, &Result::status);
  };
  std::string xml = std::format(
      "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"
      "<testsuites name=\"costar\" tests=\"{0}\" failures=\"{1}\" skipped=\"{2}\" time=\"{3:.3f}\">\n"
      "  <testsuite name=\"costar test\" tests=\"{0}\" failures=\"{1}\" errors=\"0\" skipped=\"{2}\" time=\"{3:.3f}\">\n",
      results.size(), count(Result::Status::Fail), count(Result::Status::Skip), seconds);
  for (const Result& r : results) {
    const std::string directory = r.file.parent_path().string();
    xml += std::format("    <testcase classname=\"{}\" name=\"{}\" file=\"{}\" time=\"{:.3f}\"",
                       xml_escaped(directory.empty() ? "." : directory, true), xml_escaped(r.name, true),
                       xml_escaped(r.file.string(), true), r.seconds);
    switch (r.status) {
      case Result::Status::Pass: xml += "/>\n"; break;
      case Result::Status::Skip:
        xml += std::format(">\n      <skipped message=\"{}\"/>\n    </testcase>\n",
                           xml_escaped(r.problems.empty() ? "" : r.problems.front(), true));
        break;
      case Result::Status::Fail: {
        std::string body;
        for (const std::string& problem : r.problems) body += problem + '\n';
        xml += std::format(">\n      <failure message=\"{}\">{}</failure>\n    </testcase>\n",
                           xml_escaped(r.headline, true), xml_escaped(body, false));
        break;
      }
    }
  }
  xml += "  </testsuite>\n</testsuites>\n";
  return xml;
}

// Characters safe in a directory name; the rest become '_'.
std::string file_name(std::string_view name) {
  std::string out;
  for (const char c : name) out += std::isalnum(static_cast<unsigned char>(c)) || c == '-' || c == '.' ? c : '_';
  return out.empty() || out == "." || out == ".." ? "_" + out : out;
}

// A scenario's directory under --artifacts: its name, made safe and cut to
// kArtifactNameMax characters (a file name has at most 255 bytes), then a
// hash of its file's canonical path and its whole name.  Scenarios whose
// names look alike once made safe or cut ("a/b", "a?b"), or share a name in
// different files, never share a directory, so neither removes the other's
// artifacts (in one run, or in ctest processes running side by side).
constexpr size_t kArtifactNameMax = 64;

fs::path artifact_dir(const fs::path& root, const Scenario& scenario) {
  std::error_code error;
  fs::path canonical = fs::weakly_canonical(fs::absolute(scenario.file), error);
  if (error) canonical = fs::absolute(scenario.file);
  uint64_t hash = 0xcbf29ce484222325;  // FNV-1a
  for (const std::string& part : {canonical.string(), std::string(1, '\0'), scenario.name}) {
    for (const char c : part) hash = (hash ^ static_cast<unsigned char>(c)) * 0x100000001b3;
  }
  return root / std::format("{}-{:012x}", file_name(scenario.name).substr(0, kArtifactNameMax),
                            hash & 0xffff'ffff'ffff);
}

// What a scenario leaves behind for CI (--artifacts), in <dir>/<scenario>-<hash>/:
//   trace.txt               the whole trace (text), and trace.json (Perfetto)
//   <machine>.console.txt   each machine's console text (its log records)
//   actual.trace            what the golden trace would be (with its filter)
//   diff.txt                the whole golden diff
//   summary.txt             the result, and why it failed
// Traces are written as the run goes, so a run that times out (on the
// watchdog's thread: abandon()) leaves what it had.  A passing scenario
// leaves nothing unless every one should.
class Artifacts {
 public:
  Artifacts(const fs::path& root, const Scenario& scenario, bool keep_passing)
      : dir_(artifact_dir(root, scenario)),
        keep_passing_(keep_passing),
        names_(machine_names(scenario)),
        perfetto_format_(scenario.name, names_) {
    std::error_code error;
    fs::remove_all(dir_, error);  // this scenario's, from a previous run
    fs::create_directories(dir_);
    trace_.emplace(dir_ / "trace.txt");
    perfetto_.emplace(dir_ / "trace.json");
    perfetto_->append(perfetto_format_.begin());
  }
  ~Artifacts() {
    if (committed_) return;
    // A passing scenario's: the outputs delete their own files first.
    trace_.reset();
    perfetto_.reset();
    consoles_.clear();
    std::error_code error;
    fs::remove_all(dir_, error);
  }
  Artifacts(const Artifacts&) = delete;
  Artifacts& operator=(const Artifacts&) = delete;

  const fs::path& dir() const { return dir_; }

  void record(const TraceRecord& r) {
    const std::lock_guard lock(mutex_);
    ++records_;
    trace_->append(format(r) + '\n');
    perfetto_->append(perfetto_format_.event(r));
    if (r.label == "log" && r.text) {
      std::unique_ptr<Output>& console = consoles_[r.machine];
      if (!console) console = std::make_unique<Output>(dir_ / (file_name(names_.at(r.machine)) + ".console.txt"));
      console->append(*r.text + '\n');
    }
  }

  // The run is over: keep everything if it failed (or all are kept).
  void finish(bool failed, const std::string& summary, const std::optional<std::string>& actual,
              const std::string& diff) {
    const std::lock_guard lock(mutex_);
    if (!failed && !keep_passing_) return;
    commit(summary);
    if (actual) write_file(dir_ / "actual.trace", *actual);
    if (!diff.empty()) write_file(dir_ / "diff.txt", diff);
  }

  // The run timed out: keep what there is, from the watchdog's thread.
  void abandon(const std::string& summary) noexcept {
    const std::lock_guard lock(mutex_);
    try {
      commit(summary + std::format("records so far: {} (trace.txt)\n", records_));
    } catch (...) {
      // Leave what could be written.
    }
  }

 private:
  void commit(const std::string& summary) {
    committed_ = true;
    perfetto_->append(PerfettoTrace::end());
    trace_->try_commit();
    perfetto_->try_commit();
    for (auto& [machine, console] : consoles_) console->try_commit();
    Output out(dir_ / "summary.txt");
    out.append(summary);
    out.try_commit();
  }

  fs::path dir_;
  bool keep_passing_;
  std::vector<std::string> names_;
  PerfettoTrace perfetto_format_;
  std::mutex mutex_;
  std::optional<Output> trace_;
  std::optional<Output> perfetto_;
  std::map<uint32_t, std::unique_ptr<Output>> consoles_;
  uint64_t records_ = 0;
  bool committed_ = false;
};

std::string summary_text(const Scenario& scenario, const fs::path& file, std::string_view result,
                         const std::vector<std::string>& problems) {
  std::string out = std::format("scenario: {}\nfile: {}\nresult: {}\n", scenario.name, file.string(), result);
  for (const std::string& problem : problems) out += indented(problem, "  ").replace(0, 2, "- ");
  return out;
}

// What `test` has found so far, for the JUnit report, shared with the
// watchdog's thread: a timeout writes the report too.
class Report {
 public:
  explicit Report(std::optional<fs::path> junit) : junit_(std::move(junit)) {}

  void add(Result result) {
    const std::lock_guard lock(mutex_);
    results_.push_back(std::move(result));
  }
  // While it lasts, the scenario that is running, and its artifacts if any.
  class Running {
   public:
    Running(Report& report, const Scenario& scenario, const fs::path& file, Artifacts* artifacts)
        : report_(report) {
      const std::lock_guard lock(report_.mutex_);
      report_.running_ = {&scenario, file, artifacts};
      report_.started_ = Clock::now();
    }
    ~Running() {
      const std::lock_guard lock(report_.mutex_);
      report_.running_ = {};
    }
    Running(const Running&) = delete;
    Running& operator=(const Running&) = delete;

    double seconds() const { return seconds_since(report_.started_); }

   private:
    Report& report_;
  };

  void write() {
    const std::lock_guard lock(mutex_);
    if (junit_) write_file(*junit_, junit_xml(results_, seconds_since(begun_)));
  }

  // The running scenario timed out: record it, leave its artifacts, and
  // write the report.  On the watchdog's thread.
  void time_out(const std::string& message) noexcept {
    const std::lock_guard lock(mutex_);
    if (running_.scenario == nullptr) return;
    try {
      const Scenario& scenario = *running_.scenario;
      if (running_.artifacts) {
        running_.artifacts->abandon(summary_text(scenario, running_.file, "TIMEOUT", {message}));
      }
      if (!junit_) return;
      results_.push_back({.name = scenario.name,
                          .file = running_.file,
                          .status = Result::Status::Fail,
                          .problems = {message},
                          .headline = message,
                          .seconds = seconds_since(started_)});
      Output out(*junit_);
      out.append(junit_xml(results_, seconds_since(begun_)));
      out.try_commit();
    } catch (...) {
      // The process is about to end: keep what was written.
    }
  }

 private:
  using Clock = std::chrono::steady_clock;
  static double seconds_since(Clock::time_point start) {
    return std::chrono::duration<double>(Clock::now() - start).count();
  }

  std::optional<fs::path> junit_;
  std::mutex mutex_;
  std::vector<Result> results_;
  struct {
    const Scenario* scenario = nullptr;
    fs::path file;
    Artifacts* artifacts = nullptr;
  } running_;
  Clock::time_point begun_ = Clock::now();
  Clock::time_point started_ = begun_;
};

// `arg` as one word for a POSIX shell.
std::string shell_word(std::string_view arg) {
  const bool plain = !arg.empty() && std::ranges::all_of(arg, [](char c) {
    return std::isalnum(static_cast<unsigned char>(c)) || std::string_view("_-./=:,+@%").find(c) != std::string_view::npos;
  });
  if (plain) return std::string(arg);
  std::string out = "'";
  for (const char c : arg) out += c == '\'' ? std::string("'\\''") : std::string(1, c);
  return out + "'";
}

// The `run` command line that reproduces one combination of a sweep.
std::string reproduce_command(const Options& options, const fs::path& file, const std::vector<Override>& overrides) {
  std::string out = shell_word(options.program) + " run " + shell_word(file.string());
  for (const fs::path& dir : options.image_dirs) out += " --images " + shell_word(dir.string());
  for (const Override& o : overrides) out += " --set " + shell_word(o.text());
  return out;
}

// "name [key=value, ...]": a sweep combination's name in results.
std::string combination_name(const std::string& name, const std::vector<Override>& combination) {
  std::string out = name + " [";
  for (size_t i = 0; i < combination.size(); ++i) out += (i == 0 ? "" : ", ") + combination[i].text();
  return out + "]";
}

// Run every scenario that has a golden trace or expectations, and check it.
// Results are flushed scenario by scenario, so a timeout loses none.  The
// golden trace covers what the scenario's golden filter keeps.  A scenario
// with sweeps then runs once per combination of their values; each checks
// the expectations (the golden is the base configuration's), and a failing
// one says how to reproduce it.
int test_command(const Options& options, const PlantFactories& plants) {
  int failures = 0;
  int tested = 0;
  Report report(options.junit_file);
  Watchdog watchdog(options.timeout, [&](const std::string& name) {
    const std::string message = timeout_message(*options.timeout);
    say(STDOUT_FILENO, "FAIL " + name + "\n  " + message + "\n");
    report.time_out(message);
  });

  // One run of `scenario` and its checks; with `golden_too`, against its
  // golden trace.  `reproduce` is the command that reruns a failure.
  const auto check = [&](const Scenario& scenario, const fs::path& file, bool golden_too,
                         const std::optional<std::string>& reproduce) {
    std::optional<Artifacts> artifacts;
    if (options.artifacts_dir) artifacts.emplace(*options.artifacts_dir, scenario, options.all_artifacts);
    const Report::Running running(report, scenario, file, artifacts ? &*artifacts : nullptr);
    watchdog.start(scenario.name);
    Trace::Sink sink;
    if (artifacts) sink = [&artifacts](const TraceRecord& r) { artifacts->record(r); };
    const Run run = run_scenario(scenario, plants, std::nullopt, false, std::move(sink));
    ++tested;
    std::vector<std::string> problems = stall_failures(scenario, run.stalls);
    std::ranges::move(stack_failures(scenario, run.overruns), std::back_inserter(problems));
    std::ranges::move(check_expectations(scenario, run.trace), std::back_inserter(problems));
    std::optional<std::string> actual;
    GoldenCheck golden;
    if (scenario.expected_trace && golden_too) {
      actual = run.trace.format(scenario.golden_filter);
      if (options.update) {
        write_file(*scenario.expected_trace, *actual);
        std::cout << "UPDATED " << scenario.name << '\n' << std::flush;
      } else if (golden = check_golden(scenario, *actual, artifacts.has_value()); !golden.problem.empty()) {
        problems.insert(problems.begin(), golden.problem);
      }
    }
    const bool failed = !problems.empty();
    std::string headline;
    if (failed) headline = golden.problem.empty() ? problems.front() : golden.headline;
    if (failed && reproduce) problems.push_back("reproduce: " + *reproduce);
    if (artifacts) {
      std::string summary = summary_text(scenario, file, failed ? "FAIL" : "PASS", problems);
      summary += std::format("virtual time: {} ns\nrecords: {} (trace.txt)\n", run.end, run.trace.records().size());
      artifacts->finish(failed, summary, actual, golden.full_diff);
      if (failed || options.all_artifacts) problems.push_back("artifacts: " + artifacts->dir().string());
    }
    report.add({.name = scenario.name,
                .file = file,
                .status = failed ? Result::Status::Fail : Result::Status::Pass,
                .problems = problems,
                .headline = headline,
                .seconds = running.seconds()});
    if (!failed) {
      if (!options.update || !actual) std::cout << "PASS " << scenario.name << '\n' << std::flush;
      return;
    }
    ++failures;
    std::cout << "FAIL " << scenario.name << '\n';
    for (const std::string& problem : problems) std::cout << "  " << problem << '\n';
    std::cout << std::flush;
  };

  for (const fs::path& file : scenario_files(options.paths)) {
    const Scenario scenario = load_scenario(file, options.image_dirs, options.overrides);
    const auto skip = [&](const std::string& why) {
      std::cout << "SKIP " << scenario.name << " (" << why << ")\n" << std::flush;
      report.add({.name = scenario.name, .file = file, .status = Result::Status::Skip, .problems = {why}});
    };
    if (!scenario.expected_trace && scenario.expectations.empty()) {
      skip("nothing in [expect]");
      continue;
    }
    if (!scenario.bridges.empty()) {
      skip("host bridges are not deterministic");
      continue;
    }
    check(scenario, file, true, std::nullopt);
    for (const std::vector<Override>& combination : sweep_combinations(scenario)) {
      std::vector<Override> overrides = options.overrides;
      overrides.insert(overrides.end(), combination.begin(), combination.end());
      Scenario variant = load_scenario(file, options.image_dirs, overrides);
      variant.name = combination_name(scenario.name, combination);
      check(variant, file, false, reproduce_command(options, file, overrides));
    }
  }
  report.write();
  if (tested == 0) {
    std::cout << "no scenario had anything to check\n";
    return 1;
  }
  return failures == 0 ? 0 : 1;
}

}  // namespace

int main(int argc, char** argv, const PlantFactories& plants) {
  try {
    const Options options = parse_args(argc, argv);
    int status;
    if (options.command == "run") {
      status = run_command(options, plants);
    } else if (options.command == "test") {
      status = test_command(options, plants);
    } else {
      throw UsageError("unknown command " + options.command);
    }
    if (!(std::cout << std::flush)) throw std::runtime_error("cannot write to standard output");
    return status;
  } catch (const UsageError& error) {
    std::cerr << "costar: " << error.what() << '\n' << kUsage;
    return 2;
  } catch (const std::exception& error) {
    std::cerr << "costar: " << error.what() << '\n';
    return 1;
  }
}

}  // namespace costar::cli
