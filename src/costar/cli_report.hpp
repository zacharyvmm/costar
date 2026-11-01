#pragma once

#include <chrono>
#include <cstdint>
#include <filesystem>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "costar/cli_run.hpp"
#include "costar/scenario.hpp"
#include "costar/trace.hpp"

// What the `costar` command makes of a run: why it failed, the golden check,
// and what it writes (the trace as it goes, CI artifacts, a JUnit report).
// Internal to cli.cpp.
namespace costar::cli {

// ── Why a run failed ──────────────────────────────────────────────────

// One message per stall, stack overrun and failed expectation.
std::vector<std::string> run_failures(const Scenario& scenario, const Run& run);

std::string timeout_message(std::chrono::seconds budget);

// "machine m, thread t: 1840 bytes of stack used, 12288 allowed (2048 declared)"
std::string stack_report(const Scenario& scenario, uint32_t machine, const ThreadStack& thread);

// A scenario's trace against its golden file, or the golden rewritten from it.
struct GoldenCheck {
  std::optional<std::string> actual = {};  // what the golden would be; none: the scenario has no golden
  bool updated = false;                    // the golden was rewritten (--update)
  std::string problem = {};                // "" if it matches; else the first hunks of a unified diff
  std::string headline = {};               // the problem, up to the end of its first hunk
  std::string full_diff = {};              // every hunk, when asked for
};

// With `update`, rewrite the golden; otherwise compare with it.
GoldenCheck check_golden(const Scenario& scenario, const Trace& trace, bool update, bool full_diff);

// ── Writing ───────────────────────────────────────────────────────────

// Write all of `text` to `fd`, from any thread, without the C++ streams.
void say(int fd, std::string_view text) noexcept;

// Text for standard output, or for a file.  A file is written through a new
// sibling file of our own (never an existing one), renamed into place by
// commit(): a failed write leaves the old file as it was.  Text is written as
// it comes, in blocks, so the timeout watchdog's thread can commit what a
// hung run has written so far.
class Output {
 public:
  explicit Output(std::optional<std::filesystem::path> file);
  ~Output();
  Output(const Output&) = delete;
  Output& operator=(const Output&) = delete;

  void append(std::string_view text);
  // Write out what has been appended so far.
  void sync();
  // Write out the rest; a file is renamed into place.
  void commit();
  bool try_commit() noexcept;

 private:
  static constexpr size_t kBlock = 64 * 1024;

  // A file is closed and renamed into place.
  bool finish() noexcept;
  void flush() noexcept;

  std::optional<std::filesystem::path> file_;
  std::string temporary_;
  int fd_;
  bool ok_ = true;
  bool committed_ = false;
  std::mutex mutex_;
  std::string buffer_;
};

// How `run` writes the trace (--format): costar's text, JSON lines, or only
// the console text (`log` records) as a console shows it.
enum class TraceFormat { Text, JsonLines, Console };

// Where `run` writes the trace as it goes, filtered: text, JSON lines or
// console text on standard output or in a `trace_file`, and a Perfetto file.
// Without `trace`, only the Perfetto file (a console has standard output).
// Console text is each `log` record's text, after "<machine>: " if the filter
// keeps several machines, written out line by line.
class TraceOutputs {
 public:
  TraceOutputs(const Scenario& scenario, TraceFilter filter, TraceFormat format, bool trace,
               std::optional<std::filesystem::path> trace_file, std::optional<std::filesystem::path> perfetto_file);

  void record(const TraceRecord& r);
  // Everything is written, and files are renamed into place.
  void commit();
  // What there is so far (a failed or timed-out run), from any thread.
  void try_commit() noexcept;
  // Write out what has been recorded so far (before a debugger break).
  void sync();

 private:
  TraceFilter filter_;
  std::vector<std::string> names_;
  TraceFormat format_;
  bool several_;  // machines whose console text is shown
  PerfettoTrace perfetto_;
  std::optional<Output> trace_;
  std::optional<Output> perfetto_file_;
};

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
  Artifacts(const std::filesystem::path& root, const Scenario& scenario, bool keep_passing);
  ~Artifacts();
  Artifacts(const Artifacts&) = delete;
  Artifacts& operator=(const Artifacts&) = delete;

  const std::filesystem::path& dir() const { return dir_; }

  void record(const TraceRecord& r);
  // The run is over: keep everything if it failed (or all are kept).
  void finish(bool failed, const std::string& summary, const std::optional<std::string>& actual,
              const std::string& diff);
  // The run timed out: keep what there is, from the watchdog's thread.
  void abandon(const std::string& summary) noexcept;

 private:
  void commit(const std::string& summary);

  std::filesystem::path dir_;
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

// summary.txt: the scenario, its file, its result, and each problem.
std::string summary_text(const Scenario& scenario, const std::filesystem::path& file, std::string_view result,
                         const std::vector<std::string>& problems);

// A scenario's outcome, for the JUnit report.
struct Result {
  enum class Status { Pass, Fail, Skip };
  std::string name;
  std::filesystem::path file;
  Status status;
  std::vector<std::string> problems = {};  // Fail: why; Skip: why
  std::string headline = {};               // Fail: the first problem, shortly
  double seconds = 0;
};

// What `test` has found so far, for the JUnit report, shared with the
// watchdog's thread: a timeout writes the report too.
class Report {
 public:
  explicit Report(std::optional<std::filesystem::path> junit) : junit_(std::move(junit)) {}

  void add(Result result);
  size_t count(Result::Status status);

  // While it lasts, the scenario that is running, and its artifacts if any.
  class Running {
   public:
    Running(Report& report, const Scenario& scenario, const std::filesystem::path& file, Artifacts* artifacts);
    ~Running();
    Running(const Running&) = delete;
    Running& operator=(const Running&) = delete;

    double seconds() const { return seconds_since(report_.started_); }

   private:
    Report& report_;
  };

  void write();

  // The running scenario timed out: record it, leave its artifacts, and
  // write the report.  On the watchdog's thread.
  void time_out(const std::string& message) noexcept;

 private:
  using Clock = std::chrono::steady_clock;
  static double seconds_since(Clock::time_point start) {
    return std::chrono::duration<double>(Clock::now() - start).count();
  }

  std::optional<std::filesystem::path> junit_;
  std::mutex mutex_;
  std::vector<Result> results_;
  struct {
    const Scenario* scenario = nullptr;
    std::filesystem::path file;
    Artifacts* artifacts = nullptr;
  } running_;
  Clock::time_point begun_ = Clock::now();
  Clock::time_point started_ = begun_;
};

}  // namespace costar::cli
