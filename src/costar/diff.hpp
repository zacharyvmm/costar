#pragma once

#include <cstddef>
#include <string>
#include <string_view>
#include <vector>

namespace costar {

// A line-by-line comparison of two texts (golden traces), as unified-diff
// hunks: "@@ -a,b +c,d @@", then " " context, "-" expected and "+" actual
// lines.
struct DiffOptions {
  size_t context = 3;               // unchanged lines around each change
  size_t max_hunks = 0;             // hunks to show; 0: all
  size_t max_hunk_lines = 0;        // lines to show per hunk; 0: all
  size_t max_edit_distance = 1000;  // lines; beyond this the diff is correct but not the smallest
};

struct Diff {
  std::vector<std::string> hunks;  // each one's lines, '\n'-terminated
  size_t total_hunks = 0;          // including those not shown
  bool only_line_endings = false;  // equal but for line endings (a missing final newline?)

  bool empty() const { return total_hunks == 0 && !only_line_endings; }
  // The hunks shown, then "... N more hunks" if any were left out.
  std::string text() const;
};

Diff diff_lines(std::string_view expected, std::string_view actual, const DiffOptions& options = {});

}  // namespace costar
