#!/bin/bash
# golden_trace_test.sh — compare simulator output against expected golden traces.
#
# Usage: ./golden_trace_test.sh [freertos|zephyr|all]
#
# Builds and runs the simulator with the specified RTOS backend,
# extracts the trace, and diffs against the expected trace file.
# Exits 0 on match.

set -euo pipefail

cd "$(dirname "$0")/.."

RTOS="${1:-all}"

test_freertos() {
    local expected="tests/traces/expected_queue_ping_pong.trace"
    echo "=== Building simulator ==="
    cargo build --quiet

    echo "=== Running simulator (FreeRTOS) ==="
    local actual
    actual=$(mktemp)
    trap "rm -f $actual" EXIT

    cargo run --quiet -- --golden > "$actual"

    echo "=== Comparing FreeRTOS traces ==="
    if diff -u "$expected" "$actual"; then
        echo "=== PASS: FreeRTOS trace matches expected golden output ==="
    else
        echo "=== FAIL: FreeRTOS trace differs from expected golden output ==="
        return 1
    fi
}

test_zephyr() {
    local expected="tests/traces/expected_zephyr_hello.trace"
    echo "=== Running simulator (Zephyr) ==="
    local actual
    actual=$(mktemp)
    trap "rm -f $actual" EXIT

    cargo run --quiet -- --rtos zephyr --golden > "$actual"

    echo "=== Comparing Zephyr traces ==="
    if diff -u "$expected" "$actual"; then
        echo "=== PASS: Zephyr trace matches expected golden output ==="
    else
        echo "=== FAIL: Zephyr trace differs from expected golden output ==="
        return 1
    fi
}

case "$RTOS" in
    freertos)
        test_freertos
        ;;
    zephyr)
        test_zephyr
        ;;
    all)
        test_freertos
        test_zephyr
        ;;
    *)
        echo "Usage: $0 [freertos|zephyr|all]"
        exit 1
        ;;
esac
