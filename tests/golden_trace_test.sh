#!/bin/bash
# golden_trace_test.sh — compare simulator output against expected golden trace.
#
# Usage: ./golden_trace_test.sh
#
# Builds and runs the simulator, extracts the trace, and diffs against
# the expected trace file.  Exits 0 on match.

set -euo pipefail

cd "$(dirname "$0")/.."

EXPECTED="tests/traces/expected_queue_ping_pong.trace"

echo "=== Building simulator ==="
cargo build --quiet

echo "=== Running simulator ==="
ACTUAL=$(mktemp)
trap "rm -f $ACTUAL" EXIT

cargo run --quiet -- --golden > "$ACTUAL"

echo "=== Comparing traces ==="
if diff -u "$EXPECTED" "$ACTUAL"; then
    echo "=== PASS: Trace matches expected golden output ==="
    exit 0
else
    echo "=== FAIL: Trace differs from expected golden output ==="
    exit 1
fi
