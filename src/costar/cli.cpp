#include "costar/cli.hpp"

#include <algorithm>
#include <cctype>
#include <charconv>
#include <chrono>
#include <format>
#include <iostream>
#include <optional>
#include <stdexcept>
#include <string>
#include <string_view>
#include <vector>

#include <unistd.h>

#include "costar/cli_report.hpp"
#include "costar/cli_run.hpp"

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

// --console: "machine.device", a channel of the scenario's.
Console console_of(const Scenario& scenario, const std::string& text) {
  const size_t dot = text.find('.');
  if (dot == std::string::npos) throw UsageError("--console needs machine.device, not '" + text + "'");
  const std::string machine = text.substr(0, dot);
  const std::string device = text.substr(dot + 1);
  for (uint32_t id = 0; id < scenario.machines.size(); ++id) {
    if (scenario.machines[id].name != machine) continue;
    for (const DeviceSpec& spec : scenario.machines[id].devices) {
      if (spec.name == device && spec.kind == DeviceKind::Channel) return {text, {id, device}};
    }
    throw UsageError("--console: machine '" + machine + "' has no channel '" + device + "'");
  }
  throw UsageError("--console: no machine named '" + machine + "'");
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

// Why `test` skips a scenario, if it does.
std::optional<std::string> skip_reason(const Scenario& scenario) {
  if (!scenario.expected_trace && scenario.expectations.empty()) return "nothing in [expect]";
  if (!scenario.bridges.empty()) return "host bridges are not deterministic";
  return std::nullopt;
}

// ── costar run ────────────────────────────────────────────────────────

// The trace goes out as the run goes.  A run that fails or times out still
// writes the trace so far.  Stalls and failed expectations are reported on
// standard error and make the command fail.  Expectations are checked
// against the whole trace, whatever the filter.
int run_command(const Options& options, const PlantFactories& plants) {
  const Scenario scenario = load_scenario(options.paths.front(), options.image_dirs, options.overrides);
  TraceOutputs outputs(scenario, trace_filter(scenario, options), options.format,
                       /*trace=*/!options.console || options.trace_file, options.trace_file, options.perfetto_file);
  Watchdog watchdog(options.timeout, [&](const std::string& name) {
    restore_terminal();
    // The message first: committing the output may block.
    say(STDERR_FILENO, "costar: " + name + ": " + timeout_message(*options.timeout) + "\n");
    outputs.try_commit();
  });
  watchdog.start(scenario.name);
  RunSettings settings{.until = options.until,
                       .realtime = options.realtime,
                       .sink = [&outputs](const TraceRecord& r) { outputs.record(r); },
                       .break_at = options.break_at,
                       .before_break = [&outputs] { outputs.sync(); }};
  for (const std::string& spec : options.break_on) settings.break_on.push_back(parse_break_on(spec, scenario));

  Run run;
  try {
    if (options.console) settings.console = console_of(scenario, *options.console);
    run = run_scenario(scenario, plants, settings);
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
  const std::vector<std::string> failed = run_failures(scenario, run);
  for (const std::string& failure : failed) std::cerr << "costar: " << failure << '\n';
  return failed.empty() ? 0 : 1;
}

// ── costar test ───────────────────────────────────────────────────────

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

// What every scenario of one `costar test` shares.
struct Test {
  const Options& options;
  const PlantFactories& plants;
  Report& report;
  Watchdog& watchdog;
};

// PASS or FAIL, and why.  A scenario whose golden was rewritten said
// UPDATED already, and passing, says nothing more.
void print_result(const Result& result, bool updated) {
  if (result.status == Result::Status::Pass) {
    if (!updated) std::cout << "PASS " << result.name << '\n';
  } else {
    std::cout << "FAIL " << result.name << '\n';
    for (const std::string& problem : result.problems) std::cout << "  " << problem << '\n';
  }
  std::cout << std::flush;
}

// Run `scenario` once, check it, and report and print the result.  With
// `golden_too`, the run is checked against its golden trace (or rewrites it,
// with --update).  `reproduce` is the command that reruns a failure.
void test_scenario(const Test& test, const Scenario& scenario, const fs::path& file, bool golden_too,
                   const std::optional<std::string>& reproduce) {
  const Options& options = test.options;
  std::optional<Artifacts> artifacts;
  if (options.artifacts_dir) artifacts.emplace(*options.artifacts_dir, scenario, options.all_artifacts);
  const Report::Running running(test.report, scenario, file, artifacts ? &*artifacts : nullptr);
  test.watchdog.start(scenario.name);

  RunSettings settings;
  if (artifacts) settings.sink = [&artifacts](const TraceRecord& r) { artifacts->record(r); };
  const Run run = run_scenario(scenario, test.plants, settings);

  std::vector<std::string> problems = run_failures(scenario, run);
  GoldenCheck golden;
  if (golden_too) golden = check_golden(scenario, run.trace, options.update, artifacts.has_value());
  if (golden.updated) std::cout << "UPDATED " << scenario.name << '\n' << std::flush;  // even if artifacts then fail
  if (!golden.problem.empty()) problems.insert(problems.begin(), golden.problem);
  const bool failed = !problems.empty();
  Result result{.name = scenario.name,
                .file = file,
                .status = failed ? Result::Status::Fail : Result::Status::Pass,
                .headline = !failed ? "" : golden.problem.empty() ? problems.front() : golden.headline};
  if (failed && reproduce) problems.push_back("reproduce: " + *reproduce);

  if (artifacts) {
    std::string summary = summary_text(scenario, file, failed ? "FAIL" : "PASS", problems);
    summary += std::format("virtual time: {} ns\nrecords: {} (trace.txt)\n", run.end, run.trace.records().size());
    artifacts->finish(failed, summary, golden.actual, golden.full_diff);
    if (failed || options.all_artifacts) problems.push_back("artifacts: " + artifacts->dir().string());
  }
  result.problems = std::move(problems);
  result.seconds = running.seconds();
  test.report.add(result);
  print_result(result, golden.updated);
}

// Run every scenario that has a golden trace or expectations, and check it.
// Results are flushed scenario by scenario, so a timeout loses none.  The
// golden trace covers what the scenario's golden filter keeps.  A scenario
// with sweeps then runs once per combination of their values; each checks
// the expectations (the golden is the base configuration's), and a failing
// one says how to reproduce it.
int test_command(const Options& options, const PlantFactories& plants) {
  Report report(options.junit_file);
  Watchdog watchdog(options.timeout, [&](const std::string& name) {
    const std::string message = timeout_message(*options.timeout);
    say(STDOUT_FILENO, "FAIL " + name + "\n  " + message + "\n");
    report.time_out(message);
  });
  const Test test{options, plants, report, watchdog};

  for (const fs::path& file : scenario_files(options.paths)) {
    const Scenario scenario = load_scenario(file, options.image_dirs, options.overrides);
    if (const std::optional<std::string> why = skip_reason(scenario)) {
      std::cout << "SKIP " << scenario.name << " (" << *why << ")\n" << std::flush;
      report.add({.name = scenario.name, .file = file, .status = Result::Status::Skip, .problems = {*why}});
      continue;
    }
    test_scenario(test, scenario, file, /*golden_too=*/true, std::nullopt);
    for (const std::vector<Override>& combination : sweep_combinations(scenario)) {
      std::vector<Override> overrides = options.overrides;
      overrides.insert(overrides.end(), combination.begin(), combination.end());
      Scenario variant = load_scenario(file, options.image_dirs, overrides);
      variant.name = combination_name(scenario.name, combination);
      test_scenario(test, variant, file, /*golden_too=*/false, reproduce_command(options, file, overrides));
    }
  }
  report.write();

  const size_t failures = report.count(Result::Status::Fail);
  if (failures + report.count(Result::Status::Pass) == 0) {
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
