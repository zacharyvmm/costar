#include "costar/cli.hpp"

#include <algorithm>
#include <cerrno>
#include <charconv>
#include <chrono>
#include <condition_variable>
#include <cstdlib>
#include <fstream>
#include <functional>
#include <iterator>
#include <iostream>
#include <mutex>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

#include <sys/stat.h>
#include <unistd.h>

#include "costar/world.hpp"

namespace costar::cli {

namespace fs = std::filesystem;

namespace {

constexpr const char* kUsage =
    "usage:\n"
    "  costar run  <scenario.toml> [--images DIR]... [--until-ms N] [--trace FILE] [--realtime] [--timeout-s N]\n"
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
  std::optional<std::chrono::seconds> timeout = {};  // wall-clock budget per scenario
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
    } else if (arg == "--timeout-s") {
      options.timeout = std::chrono::seconds(parse_whole(value(), arg, 1, 1'000'000'000, "seconds (at least 1)"));
    } else if (arg.starts_with("--")) {
      throw UsageError("unknown option " + arg);
    } else {
      options.paths.emplace_back(arg);
    }
  }
  if (options.paths.empty()) throw UsageError("no scenario given");
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

// Scenarios with host bridges always run in real time.
Trace run_scenario(const Scenario& scenario, const PlantFactories& plants, std::optional<Time> until,
                   bool realtime, Trace::Sink sink = {}) {
  const Time end = until.value_or(scenario.duration.value_or(kNever));
  realtime = realtime || !scenario.bridges.empty();
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
  if (realtime) {
    world.run_realtime(end);
  } else {
    world.run_until(end);
  }
  return world.trace();
}

std::string read_file(const fs::path& file) {
  std::ifstream in(file, std::ios::binary);
  std::string text;
  if (in && fs::is_regular_file(file)) text.assign(std::istreambuf_iterator<char>(in), {});
  if (!in || in.bad() || !fs::is_regular_file(file)) throw std::runtime_error("cannot read " + file.string());
  return text;
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

 private:
  static constexpr size_t kBlock = 64 * 1024;

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

// Write all of `text` to `fd`, from any thread, without the C++ streams.
void say(int fd, const std::string& text) {
  for (size_t done = 0; done < text.size();) {
    const ssize_t n = write(fd, text.data() + done, text.size() - done);
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) return;
    done += static_cast<size_t>(n);
  }
}

// Ends the process when a scenario runs longer than its wall-clock budget.
// Firmware in a busy loop that makes no costar calls never gives control
// back, so only another thread can notice.  `expire` runs on the watchdog's
// thread, with the scenario's name, just before the process exits with
// status 1.
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
        expire_(scenario_);
        std::_Exit(1);
      }
    }
  }

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
std::vector<std::string> stall_failures(const Scenario& scenario, const Trace& trace) {
  std::vector<std::string> out;
  for (const TraceRecord& r : trace.records()) {
    if (r.label != "stall") continue;
    out.push_back("machine " + scenario.machines.at(r.machine).name + " stalled at " + std::to_string(r.at) +
                  " ns: its firmware made more than " + std::to_string(scenario.stall_limit.value_or(kDefaultStallLimit)) +
                  " costar calls without letting time pass (a busy loop? build it with INSTRUMENT, or call "
                  "costar_cpu_poll() in the loop; or raise stall_limit)");
  }
  return out;
}

// "line N: expected ... / got ..." for the first difference.
std::string first_difference(const std::string& expected, const std::string& actual) {
  std::istringstream e(expected), a(actual);
  std::string expected_line, actual_line;
  for (int line = 1;; ++line) {
    const bool more_expected = static_cast<bool>(std::getline(e, expected_line));
    const bool more_actual = static_cast<bool>(std::getline(a, actual_line));
    if (!more_expected && !more_actual) return "the traces differ only in line endings (a missing final newline?)";
    if (!more_expected) expected_line = "<end of trace>";
    if (!more_actual) actual_line = "<end of trace>";
    if (expected_line != actual_line) {
      return "line " + std::to_string(line) + ":\n    expected: " + expected_line + "\n    actual:   " + actual_line;
    }
  }
}

// The trace goes out as the run goes.  A run that fails or times out still
// writes the trace so far.  Stalls and failed expectations are reported on
// standard error and make the command fail.
int run_command(const Options& options, const PlantFactories& plants) {
  const Scenario scenario = load_scenario(options.paths.front(), options.image_dirs);
  Output output(options.trace_file);
  Watchdog watchdog(options.timeout, [&](const std::string& name) {
    output.try_commit();
    say(STDERR_FILENO, "costar: " + name + ": " + timeout_message(*options.timeout) + "\n");
  });
  watchdog.start(scenario.name);
  Trace trace;
  try {
    trace = run_scenario(scenario, plants, options.until, options.realtime,
                         [&output](const TraceRecord& r) { output.append(format(r) + '\n'); });
  } catch (...) {
    output.try_commit();
    throw;
  }
  output.commit();
  std::vector<std::string> failed = stall_failures(scenario, trace);
  std::ranges::move(check_expectations(scenario, trace), std::back_inserter(failed));
  for (const std::string& failure : failed) std::cerr << "costar: " << failure << '\n';
  return failed.empty() ? 0 : 1;
}

// Why a scenario's trace fails its golden file, or "" if it matches.
std::string golden_failure(const Scenario& scenario, const std::string& actual) {
  std::string expected;
  try {
    expected = read_file(*scenario.expected_trace);
  } catch (const std::exception& error) {
    return error.what();
  }
  if (actual == expected) return "";
  return scenario.expected_trace->string() + "\n    " + first_difference(expected, actual);
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
      std::cout << "SKIP " << scenario.name << " (nothing in [expect])\n";
      continue;
    }
    if (!scenario.bridges.empty()) {
      std::cout << "SKIP " << scenario.name << " (host bridges are not deterministic)\n";
      continue;
    }
    watchdog.start(scenario.name);
    const Trace trace = run_scenario(scenario, plants, std::nullopt, false);
    ++tested;
    std::vector<std::string> problems = stall_failures(scenario, trace);
    std::ranges::move(check_expectations(scenario, trace), std::back_inserter(problems));
    if (scenario.expected_trace) {
      const std::string actual = trace.format();
      if (options.update) {
        write_file(*scenario.expected_trace, actual);
        std::cout << "UPDATED " << scenario.name << '\n';
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
