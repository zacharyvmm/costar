#!/bin/bash
# golden_trace_test.sh — compare simulator output against expected golden traces.
#
# Usage: ./golden_trace_test.sh [freertos|zephyr|broader-api|zephyr-broader-api|all]
#
# Builds and runs the simulator, extracts the trace, and diffs against
# the expected trace file.  Exits 0 on match.

set -euo pipefail

cd "$(dirname "$0")/.."

RTOS="${1:-freertos}"

# On Windows, cargo run output may have CRLF line endings while the
# expected golden trace files have LF.  Strip CR for comparison.
# Use tr (more portable than sed \r across platforms).
strip_cr() {
    tr -d '\r' < "$1"
}

run_golden_test() {
    local rtos_label="$1"
    local expected_file="$2"
    shift 2
    local extra_args=("$@")

    echo "=== Building simulator ==="
    ZEPHYR_BASE="${ZEPHYR_BASE:-}" cargo build --quiet ${ZEPHYR_BASE:+--features zephyr_real}

    echo "=== Running simulator ($rtos_label) ==="
    ACTUAL=$(mktemp)
    ACTUAL_CLEAN=$(mktemp)
    trap "rm -f $ACTUAL $ACTUAL_CLEAN" EXIT

    ZEPHYR_BASE="${ZEPHYR_BASE:-}" ZEPHYR_APP="${ZEPHYR_APP:-}" cargo run ${ZEPHYR_BASE:+--features zephyr_real} --quiet -- --golden ${extra_args[@]+"${extra_args[@]}"} > "$ACTUAL"

    # Normalize line endings for comparison.
    strip_cr "$ACTUAL" > "$ACTUAL_CLEAN"

    echo "=== Comparing traces ($rtos_label) ==="
    if diff -u "$expected_file" "$ACTUAL_CLEAN"; then
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
    broader-api)
        run_golden_test "Broader-API" "tests/traces/expected_broader_api.trace" --mode broader-api
        ;;
    zephyr-broader-api)
        if [ -z "${ZEPHYR_BASE:-}" ]; then
            echo "=== SKIP (Zephyr-Broader-API): ZEPHYR_BASE not set (requires real Zephyr source) ==="
            exit 0
        fi
        run_golden_test "Zephyr-Broader-API" "tests/traces/expected_zephyr_broader_api.trace" \
            --rtos zephyr --mode broader-api
        ;;
    all)
        run_golden_test "FreeRTOS" "tests/traces/expected_queue_ping_pong.trace"
        FRET=$?
        run_golden_test "Zephyr" "tests/traces/expected_zephyr_hello.trace" --rtos zephyr
        ZRET=$?
        run_golden_test "Broader-API" "tests/traces/expected_broader_api.trace" --mode broader-api
        BRET=$?
        if [ -n "${ZEPHYR_BASE:-}" ]; then
            run_golden_test "Zephyr-Broader-API" "tests/traces/expected_zephyr_broader_api.trace" \
                --rtos zephyr --mode broader-api
            ZBRET=$?
        else
            echo "=== SKIP (Zephyr-Broader-API): ZEPHYR_BASE not set ==="
            ZBRET=0
        fi
        if [ $FRET -eq 0 ] && [ $ZRET -eq 0 ] && [ $BRET -eq 0 ] && [ $ZBRET -eq 0 ]; then
            echo "=== ALL PASS ==="
            exit 0
        else
            echo "=== SOME FAILED ==="
            exit 1
        fi
        ;;
    *)
        echo "Usage: $0 [freertos|zephyr|broader-api|zephyr-broader-api|all]"
        exit 1
        ;;
esac
