#!/usr/bin/env bash
# Golden trace test for scenario files.
#
# Runs each scenario in --golden mode and compares the output against
# the expected trace file.
#
# Usage:
#   bash tests/scenario_golden_test.sh              # Run all scenarios
#   bash tests/scenario_golden_test.sh ping_pong     # Run specific scenario

set -eo pipefail

cd "$(dirname "$0")/.."

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'
PASS=0
FAIL=0

strip_cr() {
    tr -d '\r'
}

run_scenario_test() {
    local name="${1:-}"
    local scenario_file="tests/scenarios/${name}.toml"
    local trace_file="tests/traces/expected_${name}.trace"

    if [ ! -f "$scenario_file" ]; then
        echo "SKIP: scenario file not found: $scenario_file"
        return
    fi

    echo -n "SCENARIO ${name}: "

    # Run in golden mode.  Cargo build warnings go to stderr, trace lines to stdout.
    local actual
    actual=$(cargo run -- --scenario "$scenario_file" --golden 2>/dev/null) || {
        echo -e "${RED}FAIL (simulator crashed)${NC}"
        FAIL=$((FAIL + 1))
        return
    }

    local actual_clean expected_clean
    actual_clean=$(mktemp)
    echo "$actual" | strip_cr > "$actual_clean"

    expected_clean=$(mktemp)
    strip_cr < "$trace_file" > "$expected_clean"

    if diff -u "$expected_clean" "$actual_clean" > /dev/null 2>&1; then
        echo -e "${GREEN}PASS${NC}"
        PASS=$((PASS + 1))
    else
        echo -e "${RED}FAIL${NC}"
        echo "=== diff (expected vs actual) ==="
        diff -u "$expected_clean" "$actual_clean" || true
        FAIL=$((FAIL + 1))
    fi

    rm -f "$actual_clean" "$expected_clean"
}

if [ $# -eq 0 ]; then
    for f in tests/scenarios/*.toml; do
        nm=$(basename "$f" .toml)
        run_scenario_test "$nm"
    done
else
    for nm in "$@"; do
        run_scenario_test "$nm"
    done
fi

echo ""
echo "Scenario golden trace tests: ${PASS} passed, ${FAIL} failed"
if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
exit 0
