#!/usr/bin/env python3
"""PreToolUse(Edit|Write|MultiEdit) gate + landing/nightly scan: no `#[ignore]` on tests.

An ignored test is a red gate made invisible. It rots silently, the feature
ships "verified", and Peter finds the black frame. SCENE_LOOP shipped with its
only real-import render gate `#[ignore]`d and failing (diff=149) — the ignore
attribute was the whole mechanism of the failure. A flaky or nondeterministic
gate gets fixed (seed control, like the RT noise gate) or deleted, never
muted.

Deterministic contract (this docstring is the spec):

  HOOK    PreToolUse on Edit/Write/MultiEdit. Scope: `.rs` files anywhere in
          the repo. Edit/MultiEdit: the inserted text (new_string) must not
          contain an ignore line. Write: an ignore line is denied only when
          it is NEW relative to the file on disk (carried-forward lines in a
          full-file rewrite stay valid — migration is on-touch via the sweep,
          not by blocking unrelated rewrites).

  LINE    An "ignore line" matches ^\\s*#\\s*\\[\\s*ignore\\b — covers
          `#[ignore]` and `#[ignore = "reason"]`.

  SCAN    `--scan` (landing_gate.py, trunk_health.py): walks `crates/**/*.rs`,
          counts ignore lines per file, compares against
          `.claude/hooks/ignored_tests_baseline.txt` (`path<TAB>count`).
          FAIL (exit 1) when any file exceeds its baseline count or a file
          not in the baseline has any. Shrinkage is always fine — the
          baseline only ratchets down (regenerate with `--record` in the same
          commit that removes ignores). Baseline lines for files that dropped
          to zero are stale-safe.

  AUDIT   `--audit` prints every occurrence (exit 0) — the sweep worklist.

Fails open on any error in hook mode; fails CLOSED in scan mode (a gate that
can't read its baseline is red, not silent).

Obsolete when: every gate runs deterministic in the default suite and the
baseline file is empty.
"""
import json
import re
import subprocess
import sys
from pathlib import Path

IGNORE_RE = re.compile(r"^\s*#\s*\[\s*ignore\b")
BASELINE_NAME = "ignored_tests_baseline.txt"

DENY_REASON = (
    "No `#[ignore]` on tests (CLAUDE.md hard rule; spec: ignored-test-guard.py "
    "docstring). An ignored gate is a red gate made invisible — SCENE_LOOP "
    "shipped broken behind one. Fix the flakiness (seed control, like the RT "
    "noise gate) or delete the test. Removing ignores is always allowed."
)


def _repo_root():
    # The repo is the one the script lives in — never cwd (a session running
    # the script by absolute path from another checkout must still scan the
    # script's own tree). git fallback only for odd layouts.
    derived = Path(__file__).resolve().parents[2]
    if (derived / "crates").is_dir():
        return derived
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True, text=True, timeout=5,
        )
        if out.returncode == 0:
            return Path(out.stdout.strip())
    except Exception:
        pass
    return derived


def _ignore_lines(text):
    return [ln for ln in text.splitlines() if IGNORE_RE.match(ln)]


def hook_main():
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return 0  # fail open
    tool = payload.get("tool_name", "")
    ti = payload.get("tool_input", {})
    path = ti.get("file_path", "") or ""
    if not path.endswith(".rs"):
        return 0

    added = []
    if tool == "Edit":
        added = _ignore_lines(ti.get("new_string", "") or "")
    elif tool == "MultiEdit":
        for edit in ti.get("edits", []) or []:
            added.extend(_ignore_lines(edit.get("new_string", "") or ""))
    elif tool == "Write":
        new_lines = _ignore_lines(ti.get("content", "") or "")
        if new_lines:
            try:
                old = Path(path).read_text()
            except Exception:
                old = ""
            old_lines = _ignore_lines(old)
            # New = occurrences beyond what the old file already carried
            # (multiset difference; carried-forward lines stay valid).
            remaining = list(old_lines)
            for ln in new_lines:
                if ln in remaining:
                    remaining.remove(ln)
                else:
                    added.append(ln)
    if not added:
        return 0
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": DENY_REASON,
        }
    }))
    return 0


def _current_counts(root):
    counts = {}
    for rs in (root / "crates").rglob("*.rs"):
        if "/target/" in str(rs):
            continue
        try:
            n = len(_ignore_lines(rs.read_text()))
        except Exception:
            continue
        if n:
            counts[str(rs.relative_to(root))] = n
    return counts


def _read_baseline(root):
    baseline = {}
    path = root / ".claude" / "hooks" / BASELINE_NAME
    for ln in path.read_text().splitlines():  # fails closed: a missing baseline raises
        ln = ln.strip()
        if not ln or ln.startswith("#"):
            continue
        file, _, count = ln.partition("\t")
        baseline[file.strip()] = int(count)
    return baseline


def scan_main(record=False, audit=False):
    root = _repo_root()
    counts = _current_counts(root)
    if audit:
        for f, n in sorted(counts.items()):
            print(f"{n:3d}  {f}")
        print(f"total: {sum(counts.values())} ignored tests in {len(counts)} files")
        return 0
    if record:
        path = root / ".claude" / "hooks" / BASELINE_NAME
        lines = ["# path<TAB>ignore-count — ratchet only shrinks; regenerate via --record",
                 "# in the same commit that removes ignores. Spec: ignored-test-guard.py."]
        lines += [f"{f}\t{n}" for f, n in sorted(counts.items())]
        path.write_text("\n".join(lines) + "\n")
        print(f"recorded {sum(counts.values())} ignores across {len(counts)} files")
        return 0
    baseline = _read_baseline(root)
    failures = []
    for f, n in sorted(counts.items()):
        allowed = baseline.get(f, 0)
        if n > allowed:
            failures.append(f"{f}: {n} ignored (baseline {allowed})")
    if failures:
        print("IGNORED-TEST GATE: new #[ignore] occurrences detected — fix or delete the test, never mute it:")
        for f in failures:
            print(f"  {f}")
        return 1
    stale = sum(1 for f, n in baseline.items() if counts.get(f, 0) < n)
    print(f"ignored-test gate: {sum(counts.values())} ignores within baseline "
          f"({stale} baseline entries shrank — rerun with --record to ratchet down)")
    return 0


if __name__ == "__main__":
    args = sys.argv[1:]
    if "--scan" in args or "--audit" in args or "--record" in args:
        sys.exit(scan_main(record="--record" in args, audit="--audit" in args))
    sys.exit(hook_main())
