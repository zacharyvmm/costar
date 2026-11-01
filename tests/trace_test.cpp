#include <doctest/doctest.h>

#include <random>
#include <string>
#include <vector>

#include "costar/diff.hpp"
#include "costar/trace.hpp"

using costar::TraceFilter;
using costar::TraceRecord;

TEST_CASE("a trace filter keeps records by machine and label") {
  const TraceRecord got{1, 0, "got_ping", 1};
  const TraceRecord tx{1, 1, "tx", 2, "link 5001"};
  const TraceRecord log{1, 1, "log", 0, "hi"};

  CHECK(TraceFilter{}.empty());
  CHECK(TraceFilter{}.keeps(tx));
  const TraceFilter machine{.machines = {1}};
  CHECK_FALSE(machine.keeps(got));
  CHECK(machine.keeps(tx));
  const TraceFilter labels{.labels = {"tx", "got_*"}};
  CHECK(labels.keeps(got));
  CHECK(labels.keeps(tx));
  CHECK_FALSE(labels.keeps(log));
  const TraceFilter both{.machines = {1}, .labels = {"got_*", "log"}};
  CHECK_FALSE(both.keeps(got));
  CHECK(both.keeps(log));
  const TraceFilter excluded{.excluded_labels = {"t*"}};
  CHECK(excluded.keeps(got));
  CHECK_FALSE(excluded.keeps(tx));
  costar::Trace trace;
  for (const TraceRecord& r : {got, tx, log}) trace.record(r);
  CHECK(trace.format(machine) == "1 m1 tx link 5001\n1 m1 log hi\n");
}

TEST_CASE("JSON lines take a record's text apart") {
  using costar::format_json;
  CHECK(format_json({5, 0, "got_ping", 3}, "a") == R"({"time_ns":5,"machine":"a","label":"got_ping","value":3})");
  CHECK(format_json({5, 1, "rx", 2, "uart 6869"}, "b") ==
        R"({"time_ns":5,"machine":"b","label":"rx","value":2,"device":"uart","hex":"6869"})");
  CHECK(format_json({5, 1, "tx", 0, "uart "}, "b") ==
        R"({"time_ns":5,"machine":"b","label":"tx","value":0,"device":"uart","hex":""})");
  CHECK(format_json({5, 1, "drop", 4, "link"}, "b") ==
        R"({"time_ns":5,"machine":"b","label":"drop","value":4,"device":"link"})");
  CHECK(format_json({5, 1, "mem", 2, "flash+16 0102"}, "b") ==
        R"({"time_ns":5,"machine":"b","label":"mem","value":2,"device":"flash","offset":16,"hex":"0102"})");
  CHECK(format_json({5, 1, "erase", 4096, "flash+8192"}, "b") ==
        R"({"time_ns":5,"machine":"b","label":"erase","value":4096,"device":"flash","offset":8192})");
  CHECK(format_json({5, 1, "watchdog", 1, "wdt"}, "b") ==
        R"({"time_ns":5,"machine":"b","label":"watchdog","value":1,"device":"wdt"})");
  CHECK(format_json({5, 1, "reg", 7, "gpio[1]=7"}, "b") ==
        R"({"time_ns":5,"machine":"b","label":"reg","value":7,"device":"gpio","register":1})");
  // Console text is escaped, and stays valid UTF-8 whatever the firmware wrote.
  CHECK(format_json({5, 1, "log", 0, "say \"hi\"\\\n\t\x01 caf\xc3\xa9 \xff \xed\xa0\x80"}, "m\"") ==
        "{\"time_ns\":5,\"machine\":\"m\\\"\",\"label\":\"log\",\"value\":0,"
        "\"text\":\"say \\\"hi\\\"\\\\\\n\\t\\u0001 caf\xc3\xa9 \xc3\xbf \xc3\xad\xc2\xa0\xc2\x80\"}");
}

TEST_CASE("a Perfetto trace has a track per machine, instants, and register counters") {
  const costar::PerfettoTrace perfetto("demo", {"a", "b"});
  const std::string begin = perfetto.begin();
  CHECK(begin.starts_with("[\n{\"ph\":\"M\",\"pid\":1,\"name\":\"process_name\",\"args\":{\"name\":\"demo\"}}"));
  CHECK(begin.find(R"({"ph":"M","pid":1,"tid":2,"name":"thread_name","args":{"name":"b"}})") != std::string::npos);
  CHECK(perfetto.event({1'500'250, 0, "log", 0, "hi"}) ==
        ",\n" R"({"ph":"i","s":"t","pid":1,"tid":1,"ts":1500.250,"name":"log","args":{"value":0,"text":"hi"}})");
  CHECK(perfetto.event({2'000, 1, "set", 9, "gpio[3]=9"}) ==
        ",\n" R"({"ph":"i","s":"t","pid":1,"tid":2,"ts":2.000,"name":"set","args":{"value":9,"device":"gpio","register":3}})"
        ",\n" R"({"ph":"C","pid":1,"ts":2.000,"name":"b gpio","args":{"[3]":9}})");
  CHECK(costar::PerfettoTrace::end() == "\n]\n");
}

namespace {

std::string lines(const std::vector<int>& values) {
  std::string out;
  for (const int v : values) out += std::to_string(v) + "\n";
  return out;
}

// The smallest number of lines to delete and insert, by dynamic programming.
size_t edit_distance(const std::vector<int>& a, const std::vector<int>& b) {
  std::vector<std::vector<size_t>> lcs(a.size() + 1, std::vector<size_t>(b.size() + 1, 0));
  for (size_t i = 1; i <= a.size(); ++i) {
    for (size_t j = 1; j <= b.size(); ++j) {
      lcs[i][j] = a[i - 1] == b[j - 1] ? lcs[i - 1][j - 1] + 1 : std::max(lcs[i - 1][j], lcs[i][j - 1]);
    }
  }
  return a.size() + b.size() - 2 * lcs[a.size()][b.size()];
}

}  // namespace

TEST_CASE("golden diffs are unified diffs with context") {
  using costar::diff_lines;
  CHECK(diff_lines("a\nb\n", "a\nb\n").empty());
  CHECK(diff_lines("a\nb\n", "a\nb").only_line_endings);
  CHECK(diff_lines("a\nb", "a\nb\n").text() == "the traces differ only in line endings (a missing final newline?)\n");

  const std::string expected = lines({1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15});
  const std::string actual = lines({1, 2, 3, 40, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15, 16});
  const costar::Diff diff = diff_lines(expected, actual);
  CHECK(diff.total_hunks == 2);
  CHECK(diff.text() ==
        "@@ -1,7 +1,7 @@\n 1\n 2\n 3\n-4\n+40\n 5\n 6\n 7\n"
        "@@ -11,5 +11,5 @@\n 11\n 12\n 13\n-14\n 15\n+16\n");
  // Hunks whose context would overlap or touch are one: here 9 lines apart.
  CHECK(diff_lines(expected, actual, {.context = 4}).total_hunks == 2);
  CHECK(diff_lines(expected, actual, {.context = 5}).total_hunks == 1);
  // Caps: hunks, and lines per hunk.
  CHECK(diff_lines(expected, actual, {.max_hunks = 1}).text() ==
        "@@ -1,7 +1,7 @@\n 1\n 2\n 3\n-4\n+40\n 5\n 6\n 7\n... 1 more hunk\n");
  CHECK(diff_lines(expected, actual, {.max_hunks = 1, .max_hunk_lines = 2}).text() ==
        "@@ -1,7 +1,7 @@\n 1\n 2\n... 6 more lines in this hunk\n... 1 more hunk\n");
  // An empty side is numbered by the line before it.
  CHECK(diff_lines("", "a\n").text() == "@@ -0,0 +1,1 @@\n+a\n");
  CHECK(diff_lines("a\nb\n", "a\n", {.context = 0}).text() == "@@ -2,1 +1,0 @@\n-b\n");
}

TEST_CASE("golden diffs are the smallest, and add up to both texts") {
  std::mt19937 random(7);
  for (int round = 0; round < 300; ++round) {
    std::vector<int> a, b;
    for (size_t i = random() % 30; i > 0; --i) a.push_back(static_cast<int>(random() % 5));
    b = a;
    for (size_t edits = random() % 8; edits > 0; --edits) {
      if (!b.empty() && random() % 2 == 0) {
        b.erase(b.begin() + static_cast<long>(random() % b.size()));
      } else {
        b.insert(b.begin() + static_cast<long>(random() % (b.size() + 1)), static_cast<int>(random() % 5));
      }
    }
    // One hunk with all the context: its lines rebuild both sides.
    const costar::Diff diff = costar::diff_lines(lines(a), lines(b), {.context = 1000});
    std::string rebuilt_a, rebuilt_b;
    size_t edits = 0;
    for (const std::string& hunk : diff.hunks) {
      for (size_t at = hunk.find('\n') + 1; at < hunk.size();) {
        const size_t end = hunk.find('\n', at);
        const std::string line = hunk.substr(at + 1, end - at - 1) + "\n";
        if (hunk[at] != '+') rebuilt_a += line;
        if (hunk[at] != '-') rebuilt_b += line;
        edits += hunk[at] != ' ';
        at = end + 1;
      }
    }
    if (a == b) continue;
    CHECK(rebuilt_a == lines(a));
    CHECK(rebuilt_b == lines(b));
    CHECK(edits == edit_distance(a, b));
  }
  // Past the edit limit the diff is still right, only not the smallest.
  const costar::Diff big = costar::diff_lines(lines({1, 2, 3, 4}), lines({5, 1, 6, 3, 7}), {.max_edit_distance = 1});
  CHECK(big.text() == "@@ -1,4 +1,5 @@\n-1\n-2\n-3\n-4\n+5\n+1\n+6\n+3\n+7\n");
}
