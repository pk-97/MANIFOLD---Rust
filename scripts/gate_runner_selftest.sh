#!/usr/bin/env bash
# Self-test for gate_runner.py (P1 gate).
#
# Runs known-pass and known-fail gates through per-lane, validates the JSONL
# output, tests no-gate/show subcommands, and probes the I2 Edit guard.
#
# Usage: bash scripts/gate_runner_selftest.sh
# Exit 0 on all passing, 1 on any failure.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
GATE_RUNNER="$SCRIPT_DIR/gate_runner.py"
VERDICTS_DIR="$REPO_DIR/.claude/orchestration/verdicts"

TASK_PASS="selftest-pass-$$"
TASK_FAIL="selftest-fail-$$"
TASK_BOTH="selftest-both-$$"
TASK_NOGATE="selftest-nogate-$$"
BRIEF_PASS=$(mktemp /tmp/gate_selftest_pass.XXXXXX.md)
BRIEF_FAIL=$(mktemp /tmp/gate_selftest_fail.XXXXXX.md)
BRIEF_BOTH=$(mktemp /tmp/gate_selftest_both.XXXXXX.md)
PASSED=0
FAILED=0

cleanup() {
    rm -f "$BRIEF_PASS" "$BRIEF_FAIL" "$BRIEF_BOTH"
    rm -f "$VERDICTS_DIR/$TASK_PASS.jsonl" "$VERDICTS_DIR/$TASK_FAIL.jsonl"
    rm -f "$VERDICTS_DIR/$TASK_BOTH.jsonl" "$VERDICTS_DIR/$TASK_NOGATE.jsonl"
    rmdir "$VERDICTS_DIR" 2>/dev/null || true
}
trap cleanup EXIT

ok()   { echo "  PASS: $1"; PASSED=$((PASSED + 1)); }
fail() { echo "  FAIL: $1"; FAILED=$((FAILED + 1)); }

# --- Build scratch briefs ---

cat > "$BRIEF_PASS" << 'EOF'
# Selftest: passing gate

**Gate:** A gate that must pass.
`true`
EOF

cat > "$BRIEF_FAIL" << 'EOF'
# Selftest: failing gate

**Gate:** A gate that must fail.
`false`
EOF

cat > "$BRIEF_BOTH" << 'EOF'
# Selftest: both gates

**Gate:** One pass, one fail.
`true`
`false`
EOF

echo "=== gate_runner self-test ==="
echo ""

# ===== Test 1: passing gate → exit 0, pass=true =====
echo "--- Test 1: passing gate ---"
OUT=$("$GATE_RUNNER" per-lane \
    --task "$TASK_PASS" \
    --brief "$BRIEF_PASS" \
    --branch "lane/test" \
    --commit "abc123" 2>&1) && RC=$? || RC=$?
if [ "$RC" -eq 0 ]; then ok "exit 0"; else fail "exit $RC (expected 0): $OUT"; fi
echo "$OUT" | grep -q "PASS" && ok "summary shows PASS" || fail "summary missing PASS"
echo "$OUT" | grep -q "true" && ok "summary mentions 'true' command" || fail "summary missing 'true'"
echo ""

# ===== Test 2: failing gate → exit 1, pass=false =====
echo "--- Test 2: failing gate ---"
OUT=$("$GATE_RUNNER" per-lane \
    --task "$TASK_FAIL" \
    --brief "$BRIEF_FAIL" \
    --branch "lane/test" \
    --commit "def456" 2>&1) && RC=$? || RC=$?
if [ "$RC" -eq 1 ]; then ok "exit 1"; else fail "exit $RC (expected 1): $OUT"; fi
echo "$OUT" | grep -q "FAIL" && ok "summary shows FAIL" || fail "summary missing FAIL"
echo ""

# ===== Test 3: mixed gates (one pass, one fail) =====
echo "--- Test 3: mixed gates ---"
OUT=$("$GATE_RUNNER" per-lane \
    --task "$TASK_BOTH" \
    --brief "$BRIEF_BOTH" \
    --branch "lane/test" \
    --commit "ghi789" 2>&1) && RC=$? || RC=$?
if [ "$RC" -eq 1 ]; then ok "exit 1 (mixed result)"; else fail "exit $RC (expected 1)"; fi
echo "$OUT" | grep -q "1/2" && ok "summary shows 1/2" || { echo "  DEBUG: summary output was: $OUT"; fail "summary shows 1/2"; }
echo ""

# ===== Test 4: JSONL validation =====
echo "--- Test 4: JSONL line-by-line validation ---"
PASS_JSONL="$VERDICTS_DIR/$TASK_PASS.jsonl"
FAIL_JSONL="$VERDICTS_DIR/$TASK_FAIL.jsonl"
BOTH_JSONL="$VERDICTS_DIR/$TASK_BOTH.jsonl"

for f in "$PASS_JSONL" "$FAIL_JSONL" "$BOTH_JSONL"; do
    if [ ! -f "$f" ]; then fail "JSONL file missing: $f"; continue; fi
    while IFS= read -r line; do
        [ -z "$line" ] && continue
        if echo "$line" | python3 -m json.tool > /dev/null 2>&1; then
            ok "valid JSON: $(basename "$f")"
        else
            fail "invalid JSON in $(basename "$f"): $line"
        fi
    done < "$f"
done
echo ""

# ===== Test 5: schema field checks =====
echo "--- Test 5: schema field correctness ---"
# Verify pass=true for the passing task
PASS_FIELDS=$(python3 -c "
import json
with open('$PASS_JSONL') as f:
    v = json.loads(f.readline())
print(json.dumps({'pass': v['pass'], 'kind': v['kind'], 'schema': v['schema'], 'phase': v['phase'], 'branch': v['branch'], 'commit': v['commit'], 'n_gates': len(v['gates']), 'runner': v['runner']}))
")
echo "  pass=true: $PASS_FIELDS"
echo "$PASS_FIELDS" | python3 -c "import sys,json; v=json.load(sys.stdin); assert v['pass']==True, 'pass should be true'; assert v['kind']=='gate'; assert v['schema']==1; assert v['phase']=='per-lane'; assert v['runner']=='gate_runner.py@lead'; assert v['branch']=='lane/test'; assert v['commit']=='abc123'; assert v['n_gates']==1" \
    && ok "pass=true fields correct" || fail "pass=true fields wrong"

FAIL_FIELDS=$(python3 -c "
import json
with open('$FAIL_JSONL') as f:
    v = json.loads(f.readline())
print(json.dumps({'pass': v['pass'], 'n_gates': len(v['gates']), 'gates[0].exit': v['gates'][0]['exit']}))
")
echo "  pass=false: $FAIL_FIELDS"
echo "$FAIL_FIELDS" | python3 -c "import sys,json; v=json.load(sys.stdin); assert v['pass']==False, 'pass should be false'; assert v['n_gates']==1; assert v['gates[0].exit']==1" \
    && ok "pass=false fields correct" || fail "pass=false fields wrong"
echo ""

# ===== Test 6: second append doesn't modify line 1 =====
echo "--- Test 6: append immutability ---"
FIRST_HASH=$(md5 -q "$PASS_JSONL")
# Run another pass gate against the same task
"$GATE_RUNNER" per-lane --task "$TASK_PASS" --brief "$BRIEF_PASS" --branch "lane/test" --commit "second" > /dev/null 2>&1 || true
SECOND_HASH=$(md5 -q <(head -1 "$PASS_JSONL"))
if [ "$FIRST_HASH" = "$SECOND_HASH" ]; then
    ok "line 1 unchanged after second append"
else
    fail "line 1 was modified by second append"
fi
# Verify file has 2 lines now
LINE_COUNT=$(wc -l < "$PASS_JSONL" | tr -d ' ')
if [ "$LINE_COUNT" -eq 2 ]; then
    ok "JSONL has 2 lines after second append"
else
    fail "JSONL has $LINE_COUNT lines (expected 2)"
fi
echo ""

# ===== Test 7: show subcommand =====
echo "--- Test 7: show subcommand ---"
OUT=$("$GATE_RUNNER" show --task "$TASK_PASS" 2>&1)
echo "$OUT" | grep -q "PASS" && ok "show shows PASS" || fail "show missing PASS"
echo "$OUT" | grep -q "$TASK_PASS" && ok "show shows task ID" || fail "show missing task ID"
echo ""

# ===== Test 8: no-gate subcommand =====
echo "--- Test 8: no-gate subcommand ---"
OUT=$("$GATE_RUNNER" no-gate --task "$TASK_NOGATE" --reason "selftest bypass" 2>&1) && RC=$? || RC=$?
if [ "$RC" -eq 0 ]; then ok "no-gate exit 0"; else fail "no-gate exit $RC (expected 0)"; fi
echo "$OUT" | grep -q "no-gate" && ok "no-gate message printed" || fail "no-gate message missing"
# Verify via show
OUT=$("$GATE_RUNNER" show --task "$TASK_NOGATE" 2>&1)
echo "$OUT" | grep -q "PASS" && ok "no-gate show shows PASS" || fail "no-gate show missing PASS"
echo "$OUT" | grep -q "selftest" && ok "no-gate shows reason" || fail "no-gate missing reason"
echo ""

# ===== Test 9: I3 — no gate commands =====
echo "--- Test 9: I3 — brief with no Gate section ---"
BRIEF_NOGATE=$(mktemp /tmp/gate_selftest_nogate.XXXXXX.md)
cat > "$BRIEF_NOGATE" << 'EOF'
# Brief without gate

**Read-back:** Just read.
EOF
OUT=$("$GATE_RUNNER" per-lane \
    --task "selftest-no-gate-$$" \
    --brief "$BRIEF_NOGATE" \
    --branch "lane/test" 2>&1) && RC=$? || RC=$?
if [ "$RC" -eq 1 ]; then ok "I3: exit 1 for no-gate brief"; else fail "I3: exit $RC (expected 1)"; fi
echo "$OUT" | grep -q "I3:" && ok "I3: error message mentions I3" || fail "I3: missing I3 message"
rm -f "$BRIEF_NOGATE"
echo ""

# ===== P2: pre-wave checks =====

echo "--- P2 Test 1: pre-wave against live fleet ---"
OUT=$("$GATE_RUNNER" pre-wave 2>&1) && RC=$? || RC=$?
if [ "$RC" -eq 0 ]; then ok "pre-wave exit 0"; else fail "pre-wave exit $RC (expected 0): $(echo "$OUT" | tail -3)"; fi
# Must print five check lines
CHECK_COUNT=$(echo "$OUT" | grep -cE '^\s+\[(PASS|FAIL|WARN)\]' || true)
if [ "$CHECK_COUNT" -eq 5 ]; then ok "pre-wave prints 5 check lines"; else fail "pre-wave prints $CHECK_COUNT check lines (expected 5)"; fi
echo "$OUT" | grep -qE 'pre-wave: [0-9]+/5 checks passed' && ok "pre-wave summary line" || fail "pre-wave missing summary line"
echo ""

# Clean up the pre-wave verdict from live run so induced-failure test starts clean
PREWAVE_JSONL="$VERDICTS_DIR/pre-wave.jsonl"
[ -f "$PREWAVE_JSONL" ] && rm -f "$PREWAVE_JSONL"

echo "--- P2 Test 2: induced failure via bad LITELLM_URL ---"
OUT=$(LITELLM_URL="http://127.0.0.1:9/" "$GATE_RUNNER" pre-wave 2>&1) && RC=$? || RC=$?
if [ "$RC" -eq 1 ]; then ok "pre-wave exit 1 with bad litellm URL"; else fail "pre-wave exit $RC (expected 1): $(echo "$OUT" | tail -3)"; fi
echo "$OUT" | grep -q "FAIL" && ok "pre-wave output contains FAIL" || fail "pre-wave output missing FAIL"
echo "$OUT" | grep -q "liveliness" && ok "pre-wave names liveliness as failing check" || fail "pre-wave missing liveliness"
echo ""

echo "--- P2 Test 3: pre-wave verdict validates ---"
# The bad-URL run wrote a verdict; validate it via json.tool
if [ -f "$PREWAVE_JSONL" ]; then
    ok "pre-wave verdict file exists"
    PREWAVE_LINES=$(wc -l < "$PREWAVE_JSONL" | tr -d ' ')
    # Validate each line as JSON
    while IFS= read -r line; do
        [ -z "$line" ] && continue
        if echo "$line" | python3 -m json.tool > /dev/null 2>&1; then
            ok "pre-wave verdict line is valid JSON"
        else
            fail "pre-wave verdict line is NOT valid JSON: $line"
        fi
        # Verify schema fields
        echo "$line" | python3 -c "
import sys, json
v = json.loads(sys.stdin.readline())
assert v['schema'] == 1, 'schema != 1'
assert v['phase'] == 'pre-wave', 'phase != pre-wave'
assert v['kind'] == 'gate', 'kind != gate'
assert 'preflight' in v['runner'], 'runner missing preflight'
assert len(v['gates']) == 5, f'expected 5 gates, got {len(v[\"gates\"])}'
# At least one gate must be failure (the liveliness one)
assert not v['pass'], 'verdict should not pass with bad liveliness'
print('schema validation: OK')
" && ok "pre-wave verdict schema valid" || fail "pre-wave verdict schema invalid"
    done < "$PREWAVE_JSONL"
else
    fail "pre-wave verdict file missing"
fi
rm -f "$PREWAVE_JSONL"
echo ""

# ===== Summary =====
echo "=== Results: $PASSED passed, $FAILED failed ==="
if [ "$FAILED" -gt 0 ]; then
    exit 1
fi
