#!/bin/bash
# golden_trace_test.sh — compare simulator output against expected golden traces.
#
# Usage: ./golden_trace_test.sh [freertos|zephyr|broader-api|i2c-spi|can|devices|entropy|task-delete|zephyr-broader-api|zephyr-ztest|tight-loop|all]
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
    i2c-spi)
        run_golden_test "I2C-SPI" "tests/traces/expected_i2c_spi.trace" --mode i2c-spi
        ;;
    can)
        run_golden_test "CAN" "tests/traces/expected_can.trace" --mode can
        ;;
    devices)
        run_golden_test "Devices" "tests/traces/expected_devices.trace" --mode devices
        ;;
    entropy)
        run_golden_test "Entropy" "tests/traces/expected_entropy.trace" --mode entropy
        ;;
    task-delete)
        run_golden_test "Task-Delete" "tests/traces/expected_task_delete.trace" --mode task-delete
        ;;
    net)
        run_golden_test "Net" "tests/traces/expected_net.trace" --mode net
        ;;
    block)
        run_golden_test "Block" "tests/traces/expected_block.trace" --mode block
        ;;
    bt)
        run_golden_test "Bt" "tests/traces/expected_bt.trace" --mode bt
        ;;
    zephyr-broader-api)
        if [ -z "${ZEPHYR_BASE:-}" ]; then
            echo "=== SKIP (Zephyr-Broader-API): ZEPHYR_BASE not set (requires real Zephyr source) ==="
            exit 0
        fi
        run_golden_test "Zephyr-Broader-API" "tests/traces/expected_zephyr_broader_api.trace" \
            --rtos zephyr --mode broader-api
        ;;
    zephyr-ztest)
        if [ -z "${ZEPHYR_BASE:-}" ]; then
            echo "=== SKIP (Zephyr-Ztest): ZEPHYR_BASE not set (requires real Zephyr source) ==="
            exit 0
        fi
        run_golden_test "Zephyr-Ztest" "tests/traces/expected_zephyr_ztest.trace" \
            --rtos zephyr --mode ztest
        ;;
    tight-loop)
        if [ "${OSTYPE:-}" = "msys" ] || [ "${OSTYPE:-}" = "cygwin" ] || [ "${OSTYPE:-}" = "win32" ]; then
            echo "=== SKIP (Tight-Loop): Edge instrumentation not supported on Windows ==="
            exit 0
        fi
        SIM_INSTRUMENT_EDGES=1 run_golden_test "Tight-Loop" "tests/traces/expected_tight_loop.trace" --mode tight-loop
        ;;
    all)
        run_golden_test "FreeRTOS" "tests/traces/expected_queue_ping_pong.trace"
        FRET=$?
        run_golden_test "Zephyr" "tests/traces/expected_zephyr_hello.trace" --rtos zephyr
        ZRET=$?
        run_golden_test "Broader-API" "tests/traces/expected_broader_api.trace" --mode broader-api
        BRET=$?
        run_golden_test "I2C-SPI" "tests/traces/expected_i2c_spi.trace" --mode i2c-spi
        I2RET=$?
        run_golden_test "CAN" "tests/traces/expected_can.trace" --mode can
        CANRET=$?
        run_golden_test "Devices" "tests/traces/expected_devices.trace" --mode devices
        DEVRET=$?
        run_golden_test "Entropy" "tests/traces/expected_entropy.trace" --mode entropy
        ENTRET=$?
        run_golden_test "Task-Delete" "tests/traces/expected_task_delete.trace" --mode task-delete
        TDRET=$?
        run_golden_test "Net" "tests/traces/expected_net.trace" --mode net
        NETRET=$?
        run_golden_test "Block" "tests/traces/expected_block.trace" --mode block
        BLKRET=$?
        run_golden_test "Bt" "tests/traces/expected_bt.trace" --mode bt
        BTRET=$?
        if [ -n "${ZEPHYR_BASE:-}" ]; then
            run_golden_test "Zephyr-Broader-API" "tests/traces/expected_zephyr_broader_api.trace" \
                --rtos zephyr --mode broader-api
            ZBRET=$?
            run_golden_test "Zephyr-Ztest" "tests/traces/expected_zephyr_ztest.trace" \
                --rtos zephyr --mode ztest
            ZZRET=$?
        else
            echo "=== SKIP (Zephyr-Broader-API): ZEPHYR_BASE not set ==="
            ZBRET=0
            echo "=== SKIP (Zephyr-Ztest): ZEPHYR_BASE not set ==="
            ZZRET=0
        fi
        if [ "${OSTYPE:-}" = "msys" ] || [ "${OSTYPE:-}" = "cygwin" ] || [ "${OSTYPE:-}" = "win32" ]; then
            echo "=== SKIP (Tight-Loop): Edge instrumentation not supported on Windows ==="
            TRET=0
        else
            SIM_INSTRUMENT_EDGES=1 run_golden_test "Tight-Loop" "tests/traces/expected_tight_loop.trace" --mode tight-loop
            TRET=$?
        fi
        if [ $FRET -eq 0 ] && [ $ZRET -eq 0 ] && [ $BRET -eq 0 ] && [ $I2RET -eq 0 ] && [ $CANRET -eq 0 ] && [ $DEVRET -eq 0 ] && [ $ENTRET -eq 0 ] && [ $TDRET -eq 0 ] && [ $ZBRET -eq 0 ] && [ $ZZRET -eq 0 ] && [ $TRET -eq 0 ]; then
            echo "=== ALL PASS ==="
            exit 0
        else
            echo "=== SOME FAILED ==="
            exit 1
        fi
        ;;
    *)
        echo "Usage: $0 [freertos|zephyr|broader-api|i2c-spi|can|devices|entropy|task-delete|net|block|bt|zephyr-broader-api|zephyr-ztest|tight-loop|all]"
        exit 1
        ;;
esac
