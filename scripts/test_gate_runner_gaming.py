#!/usr/bin/env python3
"""Self-test for gate_runner.py gaming scan + fail-streak directive.

scan_gaming is a pure function over unified-diff text, so synthetic diffs
prove it directly. _fail_streak reads the verdict trail; a temp
GATE_RUNNER_VERDICTS_DIR proves it end-to-end.

Usage: python3 scripts/test_gate_runner_gaming.py — exit 0 iff all pass.
"""

import importlib.util
import json
import os
import sys
import tempfile
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
_TMP = tempfile.mkdtemp(prefix="gate_gaming_test_")
os.environ["GATE_RUNNER_VERDICTS_DIR"] = _TMP

_spec = importlib.util.spec_from_file_location(
    "gate_runner", str(SCRIPT_DIR / "gate_runner.py"))
gate_runner = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_spec and gate_runner)

PASSED = 0
FAILED = 0


def check(name, cond, detail=""):
    global PASSED, FAILED
    if cond:
        PASSED += 1
        print(f"  [PASS] {name}")
    else:
        FAILED += 1
        print(f"  [FAIL] {name} — {detail}")


def tripped(entries):
    return sorted(e["cmd"] for e in entries if e["exit"] != 0)


def diff(*lines):
    return "\n".join(["--- a/src/x.rs", "+++ b/src/x.rs", "@@ -1 +1 @@", *lines])


# 1. Clean diff → single passing scan entry
entries = gate_runner.scan_gaming(diff("+let x = 1;", "-let x = 0;"))
check("clean diff passes", tripped(entries) == [] and len(entries) == 1)

# 2. Deleted assertion → red
entries = gate_runner.scan_gaming(diff("-    assert_eq!(out, expected);"))
check("removed assert trips", tripped(entries) == ["gaming: removed-asserts"])

# 3. Moved assertion (removed AND re-added) → clean
entries = gate_runner.scan_gaming(
    diff("-    assert_eq!(out, expected);", "+    assert_eq!(out, expected);"))
check("moved assert is clean", tripped(entries) == [])

# 4. Deleted #[test] → red
entries = gate_runner.scan_gaming(diff("-#[test]", "-fn proves_it() {"))
check("removed #[test] trips", tripped(entries) == ["gaming: removed-tests"])

# 5. Added #[allow(...)] → red, even net-zero context
entries = gate_runner.scan_gaming(diff("+#[allow(clippy::too_many_arguments)]"))
check("added allow trips", tripped(entries) == ["gaming: added-allow"])

# 6. Added #![allow(...)] (inner attribute) → red
entries = gate_runner.scan_gaming(diff("+#![allow(dead_code)]"))
check("added inner allow trips", tripped(entries) == ["gaming: added-allow"])

# 7. Added #[ignore] → red
entries = gate_runner.scan_gaming(diff("+#[ignore]", "+#[test]"))
check("added ignore trips", tripped(entries) == ["gaming: added-ignore"])

# 8. Removing an allow (cleanup) → clean
entries = gate_runner.scan_gaming(diff("-#[allow(dead_code)]"))
check("removed allow is clean", tripped(entries) == [])

# 9. tokio test attr counts as a test
entries = gate_runner.scan_gaming(diff("-#[tokio::test]"))
check("removed tokio test trips", tripped(entries) == ["gaming: removed-tests"])

# 10. Prose mentioning assert! in a doc line — still counted only on +/- lines
entries = gate_runner.scan_gaming(
    "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n never assert!(this) context line")
check("context lines ignored", tripped(entries) == [])

# 11. Non-Rust hunks never trip — pattern text quoted in a script/doc is not
# a suppression (this very test file is the canonical false positive)
entries = gate_runner.scan_gaming("\n".join([
    "--- a/scripts/t.py", "+++ b/scripts/t.py", "@@ -1 +1 @@",
    '+entries = scan(diff("+#[allow(dead_code)]"))',
    '-    assert_eq!(out, expected);',
]))
check("non-rust hunks ignored", tripped(entries) == [])

# 12. Mixed diff: rust hunk trips, python hunk stays silent
entries = gate_runner.scan_gaming("\n".join([
    "--- a/scripts/t.py", "+++ b/scripts/t.py", "@@ -1 +1 @@",
    "+# #[ignore] in prose",
    "--- a/src/x.rs", "+++ b/src/x.rs", "@@ -1 +1 @@",
    "+#[ignore]",
]))
check("mixed diff scans rust only", tripped(entries) == ["gaming: added-ignore"])

# --- _fail_streak against a real trail ---


def verdict(passed):
    return {
        "schema": 1, "task": "BUG-test", "phase": "per-lane", "brief": "b.md",
        "branch": "lane/x", "commit": None, "gates": [], "scope":
        {"files_changed": [], "in_scope": True}, "pass": passed,
        "kind": "gate", "reason": None, "runner": "gate_runner.py@lead",
        "ts": "2026-07-27T00:00:00+00:00",
    }


trail = Path(_TMP) / "BUG-test.jsonl"
trail.parent.mkdir(parents=True, exist_ok=True)
with open(trail, "w") as f:
    for p in (True, False, False):
        f.write(json.dumps(verdict(p)) + "\n")
# gate_runner caches VERDICTS_DIR at import; point it at the temp dir.
gate_runner.VERDICTS_DIR = Path(_TMP)
check("streak counts trailing fails", gate_runner._fail_streak("BUG-test") == 2,
      f"got {gate_runner._fail_streak('BUG-test')}")

with open(trail, "a") as f:
    f.write(json.dumps(verdict(True)) + "\n")
check("streak resets on pass", gate_runner._fail_streak("BUG-test") == 0)

check("no trail = streak 0", gate_runner._fail_streak("BUG-none") == 0)

# --- _resolve_lane_commit against fake worktree slots ---

import subprocess


def git(cwd, *args):
    r = subprocess.run(["git", "-C", str(cwd), *args],
                       capture_output=True, text=True)
    assert r.returncode == 0, f"git {args} failed: {r.stderr}"
    return r.stdout.strip()


def make_slot(root, name, task_in_msg):
    slot = Path(root) / name
    slot.mkdir(parents=True)
    git(slot, "init", "-q")
    git(slot, "commit", "--allow-empty", "-q", "-m", "base")
    base = git(slot, "rev-parse", "HEAD")
    git(slot, "update-ref", "refs/remotes/origin/main", base)
    if task_in_msg:
        git(slot, "commit", "--allow-empty", "-q", "-m", f"fix thing\n\n{task_in_msg}")
    return git(slot, "rev-parse", "HEAD")


slots_root = Path(tempfile.mkdtemp(prefix="gate_slots_test_"))
tip_a = make_slot(slots_root, "slot-1", "BUG-aaaa")
make_slot(slots_root, "slot-2", None)  # slot with nothing unlanded
gate_runner.WORKTREES_DIR = slots_root

check("slot resolves by task id",
      gate_runner._resolve_lane_commit("BUG-aaaa") == tip_a)
check("unknown task resolves to none",
      gate_runner._resolve_lane_commit("BUG-zzzz") is None)

make_slot(slots_root, "slot-3", "BUG-aaaa")  # second claimant
check("ambiguous slots resolve to none",
      gate_runner._resolve_lane_commit("BUG-aaaa") is None)

# --- hook liveness checks run green against the real repo ---

entry = gate_runner._check_hooks_registered()
check("hooks registered in settings.json", entry["exit"] == 0, entry["tail"])

entry = gate_runner._check_hooks_fire()
check("worktree-guard denies the canary", entry["exit"] == 0, entry["tail"])

entry = gate_runner._check_enforcement_manifest()
check("enforcement manifest matches reality", entry["exit"] == 0, entry["tail"])
check("manifest reports soft surface", "prompt-enforced" in entry["tail"],
      entry["tail"])

# review subcommand refuses token rationales (die() → SystemExit)
import argparse
ns = argparse.Namespace(task="BUG-t", verdict="accept", subject="x",
                        rationale="ok", by="lead")
try:
    gate_runner.cmd_review(ns)
    check("review refuses token rationale", False, "no SystemExit")
except SystemExit as e:
    check("review refuses token rationale", bool(e.code), str(e.code)[:60])

print(f"\n{PASSED} passed, {FAILED} failed")
sys.exit(0 if FAILED == 0 else 1)
