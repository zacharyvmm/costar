#include "costar/cli.hpp"

#include <algorithm>
#include <cerrno>
#include <charconv>
#include <chrono>
#include <condition_variable>
#include <csignal>
#include <cstdlib>
#include <fstream>
#include <functional>
#include <future>
#include <iterator>
#include <iostream>
#include <mutex>
#include <sstream>
#include <stdexcept>
#include <string>
#include <string_view>
#include <thread>
#include <vector>

#include <sys/stat.h>
#include <termios.h>
#include <unistd.h>

#include "costar/diff.hpp"
#include "costar/world.hpp"

namespace costar::cli {

namespace fs = std::filesystem;

namespace {

constexpr const char* kUsage =
    "usage:\n"
    "  costar run  <scenario.toml> [--images DIR]... [--until-ms N] [--trace FILE] [--realtime] [--timeout-s N]\n"
    "              [--stacks] [--console MACHINE.DEVICE] [--format text|jsonl] [--perfetto FILE]\n"
    "              [--machine NAME]... [--label LABEL]... [--exclude-label LABEL]...\n"
    "  costar test <scenario.toml | DIR>... [--images DIR]... [--update] [--timeout-s N]\n";

// A problem with the command line itself.
struct UsageError : std::runtime_error {
  using std::runtime_error::runtime_error;
};

struct Options {
  std::string command;
  std::vector<fs::path> paths = {};
  std::vector<fs::path> image_dirs = {};
  std::optional<Time> until = {};
  std::optional<fs::path> trace_file = {};
  bool update = false;
  bool realtime = false;
  bool stacks = false;  // report every thread's stack use at the end
  std::optional<std::chrono::seconds> timeout = {};  // wall-clock budget per scenario
  std::optional<std::string> console = {};           // "machine.device" on standard input and output
  bool json_lines = false;                           // --format jsonl
  std::optional<fs::path> perfetto_file = {};
  std::vector<std::string> machines = {};  // the trace filter (TraceFilter), by name
  std::vector<std::string> labels = {};
  std::vector<std::string> excluded_labels = {};
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
  Options options{.command = argv[1]};
  for (int i = 2; i < argc; ++i) {
    const std::string arg = argv[i];
    auto value = [&]() -> std::string {
      if (++i >= argc) throw UsageError(arg + " needs a value");
      return argv[i];
    };
    if (arg == "--images") {
      options.image_dirs.emplace_back(value());
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
      if (format != "text" && format != "jsonl") throw UsageError("--format is text or jsonl, not '" + format + "'");
      options.json_lines = format == "jsonl";
    } else if (arg == "--perfetto") {
      options.perfetto_file = value();
    } else if (arg == "--machine") {
      options.machines.push_back(value());
    } else if (arg == "--label") {
      options.labels.push_back(value());
    } else if (arg == "--exclude-label") {
      options.excluded_labels.push_back(value());
    } else if (arg == "--timeout-s") {
      options.timeout = std::chrono::seconds(parse_whole(value(), arg, 1, 1'000'000'000, "seconds (at least 1)"));
    } else if (arg.starts_with("--")) {
      throw UsageError("unknown option " + arg);
    } else {
      options.paths.emplace_back(arg);
    }
  }
  if (options.paths.empty()) throw UsageError("no scenario given");
  if (options.command != "run") {
    for (const auto& [given, option] : {std::pair{options.console.has_value(), "--console"},
                                        {options.json_lines, "--format"},
                                        {options.perfetto_file.has_value(), "--perfetto"},
                                        {!options.machines.empty(), "--machine"},
                                        {!options.labels.empty(), "--label"},
                                        {!options.excluded_labels.empty(), "--exclude-label"}}) {
      if (given) throw UsageError(std::string(option) + " goes with run");
    }
  }
  return options;
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
};

// Scenarios with host bridges always run in real time, and so do console
// runs.  A real-time run ends early at a signal (StopSignals).  A console run
// also ends at Ctrl-], or once its input has ended and the firmware has been
// quiet for kConsoleLinger.  At the end, every thread's whole stack is
// scanned for overruns its canary missed.
Run run_scenario(const Scenario& scenario, const PlantFactories& plants, std::optional<Time> until, bool realtime,
                 Trace::Sink sink = {}, const std::optional<std::string>& console = {}) {
  const Time end = until.value_or(scenario.duration.value_or(kNever));
  realtime = realtime || !scenario.bridges.empty() || console;
  if (end == kNever && !scenario.plants.empty() && !realtime) {
    throw std::runtime_error(scenario.name + ": a scenario with a plant needs a duration");
  }
  World world;
  world.on_record(std::move(sink));
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
  Run run{world.trace(), world.stalls(), world.stack_overruns()};
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

// Where `run` writes the trace as it goes, filtered: text or JSON lines on
// standard output or in a --trace file (not on standard output if a console
// has it), and a Perfetto file.
class TraceOutputs {
 public:
  TraceOutputs(const Options& options, const Scenario& scenario)
      : filter_(trace_filter(scenario, options)),
        names_(machine_names(scenario)),
        json_lines_(options.json_lines),
        perfetto_(scenario.name, names_) {
    if (!options.console || options.trace_file) trace_.emplace(options.trace_file);
    if (options.perfetto_file) {
      perfetto_file_.emplace(*options.perfetto_file);
      perfetto_file_->append(perfetto_.begin());
    }
  }

  void record(const TraceRecord& r) {
    if (!filter_.keeps(r)) return;
    if (trace_) trace_->append((json_lines_ ? format_json(r, names_.at(r.machine)) : format(r)) + '\n');
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

 private:
  TraceFilter filter_;
  std::vector<std::string> names_;
  bool json_lines_;
  PerfettoTrace perfetto_;
  std::optional<Output> trace_;
  std::optional<Output> perfetto_file_;
};

// The trace goes out as the run goes.  A run that fails or times out still
// writes the trace so far.  Stalls and failed expectations are reported on
// standard error and make the command fail.  Expectations are checked
// against the whole trace, whatever the filter.
int run_command(const Options& options, const PlantFactories& plants) {
  const Scenario scenario = load_scenario(options.paths.front(), options.image_dirs);
  TraceOutputs outputs(options, scenario);
  Watchdog watchdog(options.timeout, [&](const std::string& name) {
    restore_terminal();
    // The message first: committing the output may block.
    say(STDERR_FILENO, "costar: " + name + ": " + timeout_message(*options.timeout) + "\n");
    outputs.try_commit();
  });
  watchdog.start(scenario.name);
  Run run;
  try {
    run = run_scenario(scenario, plants, options.until, options.realtime,
                       [&outputs](const TraceRecord& r) { outputs.record(r); }, options.console);
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

// Why a scenario's trace fails its golden file, or "" if it matches: the
// first hunks of a unified diff.
std::string golden_failure(const Scenario& scenario, const std::string& actual) {
  std::string expected;
  try {
    expected = read_file(*scenario.expected_trace);
  } catch (const std::exception& error) {
    return error.what();
  }
  const Diff diff = diff_lines(expected, actual, kGoldenDiff);
  if (diff.empty()) return "";
  std::string out = scenario.expected_trace->string() + ": the trace differs (- golden, + actual):\n";
  out += indented(diff.text(), "    ");
  out.pop_back();  // the caller ends the line
  return out;
}

// Run every scenario that has a golden trace or expectations, and check it.
// Results are flushed scenario by scenario, so a timeout loses none.
int test_command(const Options& options, const PlantFactories& plants) {
  int failures = 0;
  int tested = 0;
  Watchdog watchdog(options.timeout, [&](const std::string& name) {
    say(STDOUT_FILENO, "FAIL " + name + "\n  " + timeout_message(*options.timeout) + "\n");
  });
  for (const fs::path& file : scenario_files(options.paths)) {
    const Scenario scenario = load_scenario(file, options.image_dirs);
    if (!scenario.expected_trace && scenario.expectations.empty()) {
      std::cout << "SKIP " << scenario.name << " (nothing in [expect])\n" << std::flush;
      continue;
    }
    if (!scenario.bridges.empty()) {
      std::cout << "SKIP " << scenario.name << " (host bridges are not deterministic)\n" << std::flush;
      continue;
    }
    watchdog.start(scenario.name);
    const Run run = run_scenario(scenario, plants, std::nullopt, false);
    ++tested;
    std::vector<std::string> problems = stall_failures(scenario, run.stalls);
    std::ranges::move(stack_failures(scenario, run.overruns), std::back_inserter(problems));
    std::ranges::move(check_expectations(scenario, run.trace), std::back_inserter(problems));
    if (scenario.expected_trace) {
      const std::string actual = run.trace.format();
      if (options.update) {
        write_file(*scenario.expected_trace, actual);
        std::cout << "UPDATED " << scenario.name << '\n' << std::flush;
      } else if (std::string failure = golden_failure(scenario, actual); !failure.empty()) {
        problems.insert(problems.begin(), std::move(failure));
      }
    }
    if (problems.empty()) {
      if (!options.update || !scenario.expected_trace) std::cout << "PASS " << scenario.name << '\n' << std::flush;
      continue;
    }
    ++failures;
    std::cout << "FAIL " << scenario.name << '\n';
    for (const std::string& problem : problems) std::cout << "  " << problem << '\n';
    std::cout << std::flush;
  }
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
