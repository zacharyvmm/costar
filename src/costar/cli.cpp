#include "costar/cli.hpp"

#include <algorithm>
#include <charconv>
#include <fstream>
#include <iostream>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

#include "costar/world.hpp"

namespace costar::cli {

namespace fs = std::filesystem;

namespace {

constexpr const char* kUsage =
    "usage:\n"
    "  costar run  <scenario.toml> [--images DIR]... [--until-ms N] [--trace FILE]\n"
    "  costar test <scenario.toml | DIR>... [--images DIR]... [--update]\n";

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
};

// A whole, non-negative number of milliseconds that fits in nanoseconds.
Time parse_milliseconds(const std::string& text) {
  uint64_t ms = 0;
  const auto [end, error] = std::from_chars(text.data(), text.data() + text.size(), ms);
  if (error != std::errc() || end != text.data() + text.size() || ms > kNever / 1'000'000) {
    throw UsageError("--until-ms needs a whole number of milliseconds, not '" + text + "'");
  }
  return milliseconds(ms);
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
      options.until = parse_milliseconds(value());
    } else if (arg == "--trace") {
      options.trace_file = value();
    } else if (arg == "--update") {
      options.update = true;
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
      files.push_back(path);
      continue;
    }
    std::vector<fs::path> found;
    for (const auto& entry : fs::directory_iterator(path)) {
      if (entry.path().extension() == ".toml") found.push_back(entry.path());
    }
    std::ranges::sort(found);
    files.insert(files.end(), found.begin(), found.end());
  }
  return files;
}

Trace run_scenario(const Scenario& scenario, const PlantFactories& plants, std::optional<Time> until) {
  const Time end = until.value_or(scenario.duration.value_or(kNever));
  if (end == kNever && !scenario.plants.empty()) {
    throw std::runtime_error(scenario.name + ": a scenario with a plant needs a duration");
  }
  World world;
  build_world(scenario, world, plants);
  world.run_until(end);
  return world.trace();
}

std::string read_file(const fs::path& file) {
  std::ifstream in(file, std::ios::binary);
  std::stringstream text;
  if (!in || !(text << in.rdbuf())) throw std::runtime_error("cannot read " + file.string());
  return text.str();
}

void write_file(const fs::path& file, const std::string& content) {
  std::ofstream out(file, std::ios::binary);
  out << content;
  out.close();
  if (!out) throw std::runtime_error("cannot write " + file.string());
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

int run_command(const Options& options, const PlantFactories& plants) {
  const Scenario scenario = load_scenario(options.paths.front(), options.image_dirs);
  const std::string trace = run_scenario(scenario, plants, options.until).format();
  if (options.trace_file) {
    write_file(*options.trace_file, trace);
  } else if (!(std::cout << trace << std::flush)) {
    throw std::runtime_error("cannot write the trace to standard output");
  }
  return 0;
}

// Run every scenario that names an expected trace and compare.
int test_command(const Options& options, const PlantFactories& plants) {
  int failures = 0;
  for (const fs::path& file : scenario_files(options.paths)) {
    const Scenario scenario = load_scenario(file, options.image_dirs);
    if (!scenario.expected_trace) {
      std::cout << "SKIP " << scenario.name << " (no [expect] trace)\n";
      continue;
    }
    const std::string actual = run_scenario(scenario, plants, std::nullopt).format();
    if (options.update) {
      write_file(*scenario.expected_trace, actual);
      std::cout << "UPDATED " << scenario.name << '\n';
      continue;
    }
    std::string expected;
    try {
      expected = read_file(*scenario.expected_trace);
    } catch (const std::exception& error) {
      ++failures;
      std::cout << "FAIL " << scenario.name << ": " << error.what() << '\n';
      continue;
    }
    if (actual == expected) {
      std::cout << "PASS " << scenario.name << '\n';
    } else {
      ++failures;
      std::cout << "FAIL " << scenario.name << " (" << scenario.expected_trace->string() << ")\n  "
                << first_difference(expected, actual) << '\n';
    }
  }
  return failures == 0 ? 0 : 1;
}

}  // namespace

int main(int argc, char** argv, const PlantFactories& plants) {
  try {
    const Options options = parse_args(argc, argv);
    if (options.command == "run") return run_command(options, plants);
    if (options.command == "test") return test_command(options, plants);
    throw UsageError("unknown command " + options.command);
  } catch (const UsageError& error) {
    std::cerr << "costar: " << error.what() << '\n' << kUsage;
    return 2;
  } catch (const std::exception& error) {
    std::cerr << "costar: " << error.what() << '\n';
    return 1;
  }
}

}  // namespace costar::cli
