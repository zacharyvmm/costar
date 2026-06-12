#!/bin/bash
# golden_trace_test.sh — compare simulator output against expected golden traces.
#
# Usage: ./golden_trace_test.sh [freertos|zephyr|all]
#
# Builds and runs the simulator, extracts the trace, and diffs against
# the expected trace file.  Exits 0 on match.

set -euo pipefail

cd "$(dirname "$0")/.."

RTOS="${1:-freertos}"

run_golden_test() {
    local rtos_label="$1"
    local expected_file="$2"
    shift 2
    local extra_args=("$@")

    echo "=== Building simulator ==="
    cargo build --quiet

    echo "=== Running simulator ($rtos_label) ==="
    ACTUAL=$(mktemp)
    trap "rm -f $ACTUAL" EXIT

    cargo run --quiet -- --golden "${extra_args[@]}" > "$ACTUAL"

    echo "=== Comparing traces ($rtos_label) ==="
    if diff -u "$expected_file" "$ACTUAL"; then
        echo "=== PASS ($rtos_label): Trace matches expected golden output ==="
    else
        echo "=== FAIL ($rtos_label): Trace differs from expected golden output ==="
        return 1
    fi
}

case "$RTOS" in
    freertos)
        run_golden_test "FreeRTOS" "tests/traces/expected_queue_ping_pong.trace"
        ;;
    zephyr)
        run_golden_test "Zephyr" "tests/traces/expected_zephyr_hello.trace" --rtos zephyr
        ;;
    all)
        run_golden_test "FreeRTOS" "tests/traces/expected_queue_ping_pong.trace"
        FRET=$?
        run_golden_test "Zephyr" "tests/traces/expected_zephyr_hello.trace" --rtos zephyr
        ZRET=$?
        if [ $FRET -eq 0 ] && [ $ZRET -eq 0 ]; then
            echo "=== ALL PASS ==="
            exit 0
        else
            echo "=== SOME FAILED ==="
            exit 1
        fi
        ;;
    *)
        echo "Usage: $0 [freertos|zephyr|all]"
        exit 1
        ;;
esac
