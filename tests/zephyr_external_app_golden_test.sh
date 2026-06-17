#!/bin/bash
# zephyr_external_app_golden_test.sh — run an external Zephyr app golden trace test.
#
# Demonstrates the ZEPHYR_APP_SOURCES env var for external app compilation.
# Compiles external_blinky.c via cc crate and compares trace output
# against the expected golden file.
#
# Requires ZEPHYR_BASE (path to Zephyr source tree) set in the environment.

set -euo pipefail

cd "$(dirname "$0")/.."

EXPECTED="tests/traces/expected_zephyr_external_blinky.trace"
APP_PATH="$(realpath crates/sim-zephyr-port/config/external_blinky.c)"
ACTUAL=$(mktemp)
ACTUAL_CLEAN=$(mktemp)
trap "rm -f $ACTUAL $ACTUAL_CLEAN" EXIT

echo "=== Building & running sim-runner (external Zephyr app: external_blinky.c) ==="
ZEPHYR_BASE="${ZEPHYR_BASE:?ZEPHYR_BASE must be set}" \
ZEPHYR_APP_SOURCES="$APP_PATH" \
cargo run --features zephyr_real --quiet -- --rtos zephyr --golden > "$ACTUAL"

# Normalize line endings
tr -d '\r' < "$ACTUAL" > "$ACTUAL_CLEAN"

echo "=== Comparing traces ==="
if diff -u "$EXPECTED" "$ACTUAL_CLEAN"; then
    echo "=== PASS: External Zephyr app golden trace matches expected output ==="
else
    echo "=== FAIL: External Zephyr app golden trace differs from expected output ==="
    exit 1
fi
