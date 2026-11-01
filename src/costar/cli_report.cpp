#include "costar/cli_report.hpp"

#include <algorithm>
#include <cctype>
#include <cerrno>
#include <cstdlib>
#include <format>
#include <fstream>
#include <iterator>
#include <stdexcept>

#include <sys/stat.h>
#include <unistd.h>

#include "costar/diff.hpp"

namespace costar::cli {

namespace fs = std::filesystem;

namespace {

// Hunks of a golden trace's diff that `test` prints, and lines per hunk.
constexpr DiffOptions kGoldenDiff{.max_hunks = 5, .max_hunk_lines = 40};

std::string read_file(const fs::path& file) {
  std::ifstream in(file, std::ios::binary);
  std::string text;
  if (in && fs::is_regular_file(file)) text.assign(std::istreambuf_iterator<char>(in), {});
  if (!in || in.bad() || !fs::is_regular_file(file)) throw std::runtime_error("cannot read " + file.string());
  return text;
}

void write_file(const fs::path& file, const std::string& content) {
  Output out(file);
  out.append(content);
  out.commit();
}

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

std::vector<std::string> machine_names(const Scenario& scenario) {
  std::vector<std::string> names;
  for (const MachineSpec& machine : scenario.machines) names.push_back(machine.name);
  return names;
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

}  // namespace

// ── Why a run failed ──────────────────────────────────────────────────

std::vector<std::string> run_failures(const Scenario& scenario, const Run& run) {
  std::vector<std::string> failed = stall_failures(scenario, run.stalls);
  std::ranges::move(stack_failures(scenario, run.overruns), std::back_inserter(failed));
  std::ranges::move(check_expectations(scenario, run.trace), std::back_inserter(failed));
  return failed;
}

std::string timeout_message(std::chrono::seconds budget) {
  return "timed out after " + std::to_string(budget.count()) +
         " s of wall-clock time (firmware in a busy loop that makes no costar calls? build it with INSTRUMENT, "
         "or call costar_cpu_poll() in the loop)";
}

std::string stack_report(const Scenario& scenario, uint32_t machine, const ThreadStack& thread) {
  std::string out = "machine " + scenario.machines.at(machine).name + ", thread '" + thread.name +
                    "': " + std::to_string(thread.used) + " bytes of stack used";
  if (thread.limit != 0) out += ", " + std::to_string(thread.limit) + " allowed";
  out += thread.declared != 0 ? " (" + std::to_string(thread.declared) + " declared)" : " (size unknown)";
  return out;
}

GoldenCheck check_golden(const Scenario& scenario, const Trace& trace, bool update, bool full_diff) {
  GoldenCheck check;
  if (!scenario.expected_trace) return check;
  const std::string& actual = check.actual.emplace(trace.format(scenario.golden_filter));
  if (update) {
    write_file(*scenario.expected_trace, actual);
    check.updated = true;
    return check;
  }
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

// ── Writing ───────────────────────────────────────────────────────────

void say(int fd, std::string_view text) noexcept {
  for (size_t done = 0; done < text.size();) {
    const ssize_t n = write(fd, text.data() + done, text.size() - done);
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) return;
    done += static_cast<size_t>(n);
  }
}

Output::Output(std::optional<fs::path> file) : file_(std::move(file)), fd_(STDOUT_FILENO) {
  if (!file_) return;
  temporary_ = file_->string() + ".XXXXXX";
  fd_ = mkstemp(temporary_.data());
  if (fd_ < 0) throw std::runtime_error("cannot write " + file_->string());
  const mode_t mask = umask(0);
  umask(mask);
  ok_ = fchmod(fd_, 0666 & ~mask) == 0;
}

Output::~Output() {
  if (!file_ || committed_) return;
  close(fd_);
  std::error_code error;
  fs::remove(temporary_, error);
}

void Output::append(std::string_view text) {
  const std::lock_guard lock(mutex_);
  buffer_ += text;
  if (buffer_.size() >= kBlock) flush();
}

void Output::sync() {
  const std::lock_guard lock(mutex_);
  flush();
}

void Output::commit() {
  if (try_commit()) return;
  throw std::runtime_error(file_ ? "cannot write " + file_->string() : "cannot write to standard output");
}

bool Output::try_commit() noexcept {
  const std::lock_guard lock(mutex_);
  if (committed_) return ok_;
  committed_ = true;
  flush();
  return finish();
}

bool Output::finish() noexcept {
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

void Output::flush() noexcept {
  for (size_t done = 0; ok_ && done < buffer_.size();) {
    const ssize_t n = write(fd_, buffer_.data() + done, buffer_.size() - done);
    if (n < 0 && errno == EINTR) continue;
    ok_ = n > 0;
    done += ok_ ? static_cast<size_t>(n) : 0;
  }
  buffer_.clear();
}

TraceOutputs::TraceOutputs(const Scenario& scenario, TraceFilter filter, TraceFormat format, bool trace,
                           std::optional<fs::path> trace_file, std::optional<fs::path> perfetto_file)
    : filter_(std::move(filter)),
      names_(machine_names(scenario)),
      format_(format),
      several_((filter_.machines.empty() ? names_.size() : filter_.machines.size()) > 1),
      perfetto_(scenario.name, names_) {
  if (trace) trace_.emplace(std::move(trace_file));
  if (perfetto_file) {
    perfetto_file_.emplace(std::move(perfetto_file));
    perfetto_file_->append(perfetto_.begin());
  }
}

void TraceOutputs::record(const TraceRecord& r) {
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

void TraceOutputs::commit() {
  if (perfetto_file_) perfetto_file_->append(PerfettoTrace::end());
  if (trace_) trace_->commit();
  if (perfetto_file_) perfetto_file_->commit();
}

void TraceOutputs::try_commit() noexcept {
  if (trace_) trace_->try_commit();
  if (perfetto_file_) perfetto_file_->try_commit();
}

void TraceOutputs::sync() {
  if (trace_) trace_->sync();
  if (perfetto_file_) perfetto_file_->sync();
}

Artifacts::Artifacts(const fs::path& root, const Scenario& scenario, bool keep_passing)
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

Artifacts::~Artifacts() {
  if (committed_) return;
  // A passing scenario's: the outputs delete their own files first.
  trace_.reset();
  perfetto_.reset();
  consoles_.clear();
  std::error_code error;
  fs::remove_all(dir_, error);
}

void Artifacts::record(const TraceRecord& r) {
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

void Artifacts::finish(bool failed, const std::string& summary, const std::optional<std::string>& actual,
                       const std::string& diff) {
  const std::lock_guard lock(mutex_);
  if (!failed && !keep_passing_) return;
  commit(summary);
  if (actual) write_file(dir_ / "actual.trace", *actual);
  if (!diff.empty()) write_file(dir_ / "diff.txt", diff);
}

void Artifacts::abandon(const std::string& summary) noexcept {
  const std::lock_guard lock(mutex_);
  try {
    commit(summary + std::format("records so far: {} (trace.txt)\n", records_));
  } catch (...) {
    // Leave what could be written.
  }
}

void Artifacts::commit(const std::string& summary) {
  committed_ = true;
  perfetto_->append(PerfettoTrace::end());
  trace_->try_commit();
  perfetto_->try_commit();
  for (auto& [machine, console] : consoles_) console->try_commit();
  Output out(dir_ / "summary.txt");
  out.append(summary);
  out.try_commit();
}

std::string summary_text(const Scenario& scenario, const fs::path& file, std::string_view result,
                         const std::vector<std::string>& problems) {
  std::string out = std::format("scenario: {}\nfile: {}\nresult: {}\n", scenario.name, file.string(), result);
  for (const std::string& problem : problems) out += indented(problem, "  ").replace(0, 2, "- ");
  return out;
}

void Report::add(Result result) {
  const std::lock_guard lock(mutex_);
  results_.push_back(std::move(result));
}

size_t Report::count(Result::Status status) {
  const std::lock_guard lock(mutex_);
  return std::ranges::count(results_, status, &Result::status);
}

Report::Running::Running(Report& report, const Scenario& scenario, const fs::path& file, Artifacts* artifacts)
    : report_(report) {
  const std::lock_guard lock(report_.mutex_);
  report_.running_ = {&scenario, file, artifacts};
  report_.started_ = Clock::now();
}

Report::Running::~Running() {
  const std::lock_guard lock(report_.mutex_);
  report_.running_ = {};
}

void Report::write() {
  const std::lock_guard lock(mutex_);
  if (junit_) write_file(*junit_, junit_xml(results_, seconds_since(begun_)));
}

void Report::time_out(const std::string& message) noexcept {
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

}  // namespace costar::cli
