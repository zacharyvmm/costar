#include "costar/diff.hpp"

#include <algorithm>
#include <cstdint>
#include <format>

namespace costar {

namespace {

std::vector<std::string_view> split_lines(std::string_view text) {
  std::vector<std::string_view> lines;
  while (!text.empty()) {
    const size_t end = text.find('\n');
    if (end == std::string_view::npos) {
      lines.push_back(text);
      break;
    }
    lines.push_back(text.substr(0, end));
    text.remove_prefix(end + 1);
  }
  return lines;
}

enum class Op : uint8_t { Equal, Delete, Insert };

// Myers' O((N+M)D) shortest edit script between a and b.  If it would take
// more than `limit` edits, the rest is one deletion and one insertion: still
// a correct diff, only not the smallest.
std::vector<Op> edit_script(const std::vector<std::string_view>& a, const std::vector<std::string_view>& b,
                            size_t limit) {
  // Common ends need no search.
  size_t prefix = 0;
  while (prefix < a.size() && prefix < b.size() && a[prefix] == b[prefix]) ++prefix;
  size_t suffix = 0;
  while (suffix < a.size() - prefix && suffix < b.size() - prefix &&
         a[a.size() - 1 - suffix] == b[b.size() - 1 - suffix]) {
    ++suffix;
  }
  const auto n = static_cast<int64_t>(a.size() - prefix - suffix);
  const auto m = static_cast<int64_t>(b.size() - prefix - suffix);
  const auto x_line = [&](int64_t x) { return a[prefix + static_cast<size_t>(x)]; };
  const auto y_line = [&](int64_t y) { return b[prefix + static_cast<size_t>(y)]; };

  std::vector<Op> middle;
  const int64_t max = std::min<int64_t>(n + m, static_cast<int64_t>(limit));
  const int64_t offset = max + 1;
  std::vector<int64_t> v(static_cast<size_t>(2 * max + 3), 0);
  // Before round d, the furthest x on each diagonal k in [-(d-1), d-1]: the
  // way back needs only those.
  std::vector<std::vector<int64_t>> history;
  int64_t found = -1;
  for (int64_t d = 0; d <= max && found < 0; ++d) {
    const auto first = v.begin() + (offset - d + 1);
    history.emplace_back(first, first + (d == 0 ? 0 : 2 * d - 1));
    for (int64_t k = -d; k <= d; k += 2) {
      int64_t x = (k == -d || (k != d && v[offset + k - 1] < v[offset + k + 1])) ? v[offset + k + 1]
                                                                                  : v[offset + k - 1] + 1;
      int64_t y = x - k;
      while (x < n && y < m && x_line(x) == y_line(y)) ++x, ++y;
      v[offset + k] = x;
      if (x >= n && y >= m) {
        found = d;
        break;
      }
    }
  }
  if (found < 0) {
    middle.assign(static_cast<size_t>(n), Op::Delete);
    middle.insert(middle.end(), static_cast<size_t>(m), Op::Insert);
  } else {
    // Walk back from (n, m) through the saved rounds.
    int64_t x = n, y = m;
    for (int64_t d = found; d > 0; --d) {
      const std::vector<int64_t>& round = history[static_cast<size_t>(d)];
      const auto before = [&](int64_t k) { return round[static_cast<size_t>(k + d - 1)]; };
      const int64_t k = x - y;
      const bool down = k == -d || (k != d && before(k - 1) < before(k + 1));
      const int64_t previous_k = down ? k + 1 : k - 1;
      const int64_t previous_x = before(previous_k);
      const int64_t previous_y = previous_x - previous_k;
      while (x > previous_x && y > previous_y) {
        middle.push_back(Op::Equal);
        --x, --y;
      }
      middle.push_back(down ? Op::Insert : Op::Delete);
      x = previous_x, y = previous_y;
    }
    while (x > 0 && y > 0) {
      middle.push_back(Op::Equal);
      --x, --y;
    }
    std::ranges::reverse(middle);
  }

  std::vector<Op> script(prefix, Op::Equal);
  script.insert(script.end(), middle.begin(), middle.end());
  script.insert(script.end(), suffix, Op::Equal);
  return script;
}

}  // namespace

std::string Diff::text() const {
  if (only_line_endings) return "the traces differ only in line endings (a missing final newline?)\n";
  std::string out;
  for (const std::string& hunk : hunks) out += hunk;
  if (total_hunks > hunks.size()) {
    const size_t more = total_hunks - hunks.size();
    out += std::format("... {} more hunk{}\n", more, more == 1 ? "" : "s");
  }
  return out;
}

Diff diff_lines(std::string_view expected, std::string_view actual, const DiffOptions& options) {
  Diff diff;
  if (expected == actual) return diff;
  const std::vector<std::string_view> a = split_lines(expected);
  const std::vector<std::string_view> b = split_lines(actual);
  if (a == b) {
    diff.only_line_endings = true;
    return diff;
  }
  const std::vector<Op> script = edit_script(a, b, options.max_edit_distance);

  // Each change, with its context, is a hunk; hunks whose context would
  // overlap or touch are one.
  std::vector<std::pair<size_t, size_t>> ranges;  // [first, last) positions in the script
  for (size_t i = 0; i < script.size(); ++i) {
    if (script[i] == Op::Equal) continue;
    const size_t first = i >= options.context ? i - options.context : 0;
    size_t end = i;
    while (end < script.size() && script[end] != Op::Equal) ++end;
    const size_t last = std::min(script.size(), end + options.context);
    if (!ranges.empty() && first <= ranges.back().second) {
      ranges.back().second = last;
    } else {
      ranges.emplace_back(first, last);
    }
    i = end - 1;
  }
  diff.total_hunks = ranges.size();

  // Line numbers where each script position starts.
  size_t position = 0, line_a = 0, line_b = 0;
  for (const auto& [first, last] : ranges) {
    for (; position < first; ++position) {
      if (script[position] != Op::Insert) ++line_a;
      if (script[position] != Op::Delete) ++line_b;
    }
    if (options.max_hunks != 0 && diff.hunks.size() == options.max_hunks) break;
    size_t count_a = 0, count_b = 0;
    for (size_t i = first; i < last; ++i) {
      count_a += script[i] != Op::Insert;
      count_b += script[i] != Op::Delete;
    }
    // A unified diff numbers an empty side by the line before it.
    std::string hunk = std::format("@@ -{},{} +{},{} @@\n", count_a == 0 ? line_a : line_a + 1, count_a,
                                   count_b == 0 ? line_b : line_b + 1, count_b);
    size_t ia = line_a, ib = line_b, shown = 0;
    for (size_t i = first; i < last; ++i, ++shown) {
      if (options.max_hunk_lines != 0 && shown == options.max_hunk_lines) {
        hunk += std::format("... {} more lines in this hunk\n", last - i);
        break;
      }
      switch (script[i]) {
        case Op::Equal: hunk += std::format(" {}\n", a[ia++]), ++ib; break;
        case Op::Delete: hunk += std::format("-{}\n", a[ia++]); break;
        case Op::Insert: hunk += std::format("+{}\n", b[ib++]); break;
      }
    }
    diff.hunks.push_back(std::move(hunk));
  }
  return diff;
}

}  // namespace costar
