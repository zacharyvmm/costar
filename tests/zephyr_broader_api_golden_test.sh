#!/bin/bash
# zephyr_broader_api_golden_test.sh — run the Zephyr broader API golden trace test.
#
# Requires ZEPHYR_BASE (path to Zephyr source tree) set in the environment.
# Builds the broader-api Zephyr app via cc crate and compares trace output
# against the expected golden file.

set -euo pipefail

cd "$(dirname "$0")/.."

EXPECTED="tests/traces/expected_zephyr_broader_api.trace"
ACTUAL=$(mktemp)
ACTUAL_CLEAN=$(mktemp)
trap "rm -f $ACTUAL $ACTUAL_CLEAN" EXIT

echo "=== Building & running sim-runner (Zephyr broader API) ==="
ZEPHYR_BASE="${ZEPHYR_BASE:?ZEPHYR_BASE must be set}" \
ZEPHYR_APP="broader_api" \
cargo run --features zephyr_real --quiet -- --rtos zephyr --golden > "$ACTUAL"

# Normalize line endings
tr -d '\r' < "$ACTUAL" > "$ACTUAL_CLEAN"

echo "=== Comparing traces ==="
if diff -u "$EXPECTED" "$ACTUAL_CLEAN"; then
    echo "=== PASS: Zephyr broader API golden trace matches expected output ==="
else
    echo "=== FAIL: Zephyr broader API golden trace differs from expected output ==="
    exit 1
fi
