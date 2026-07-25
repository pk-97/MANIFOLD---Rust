#!/usr/bin/env python3
"""Gate Runtime — verdicts the machine writes, not claims the lanes make.

One mechanism, four packs (D1). Executes gate commands via subprocess
(timeout, captured exit + last-20-lines tail), writes typed verdicts to
append-only JSONL trail at .claude/orchestration/verdicts/<task>.jsonl.
Lanes never write verdicts (D2). Design: docs/GATE_RUNTIME_DESIGN.md.

Subcommands:
  per-lane --task <id> --brief <path> [--branch <name>] [--commit <sha>]
    Extract gate commands from the brief's Gate section, execute each,
    append a per-lane verdict to the trail. Exits 0 iff all pass.
  no-gate --task <id> --reason <text>
    Append a kind=no-gate verdict (explicit bypass with mandatory reason).
  show --task <id>
    Print every verdict for the task from the trail.

Verdict schema v1 (D5):
  {"schema": 1, "task": "BUG-xxx", "phase": "per-lane",
   "brief": "<path>#<anchor>", "branch": "lane/...", "commit": "sha|null",
   "gates": [{"cmd": "...", "exit": 0, "duration_s": 12.3, "tail": "last N"}],
   "scope": {"files_changed": [], "in_scope": true},
   "pass": true, "kind": "gate|no-gate", "reason": "null|str",
   "runner": "gate_runner.py@<mode>", "ts": "2026-..."}
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

SCHEMA_VERSION = 1
GATE_TIMEOUT_S = 300
TAIL_LINES = 20

REPO = Path(__file__).resolve().parent.parent
VERDICTS_DIR = REPO / ".claude" / "orchestration" / "verdicts"

SECTION_LABEL = re.compile(r"^\s*-?\s*\*\*[A-Z][A-Za-z/-]+\**\s*:")
GATE_HEADING = re.compile(r"^\s*-?\s*\*{0,2}Gate\*{0,2}\s*:")

PHASES = frozenset(["pre-wave", "pre-dispatch", "per-lane", "pre-land"])
RUNNER_MODES = frozenset(["subagent-stop", "lead", "preflight", "lint"])
KINDS = frozenset(["gate", "no-gate"])

VERDICT_FIELDS = frozenset([
    "schema", "task", "phase", "brief", "branch", "commit",
    "gates", "scope", "pass", "kind", "reason", "runner", "ts",
])
GATE_FIELDS = frozenset(["cmd", "exit", "duration_s", "tail"])


def die(msg):
    sys.exit(f"gate_runner: {msg}")


def ensure_verdicts_dir():
    VERDICTS_DIR.mkdir(parents=True, exist_ok=True)


def task_path(task_id):
    return VERDICTS_DIR / f"{task_id}.jsonl"


def _verify_schema(v):
    """Check that a verdict dict conforms to schema 1. Dies on violation.

    Validates BOTH existing trail lines (I4: unparseable = stop, never
    skip) AND the verdict about to be appended.
    """
    if not isinstance(v, dict):
        die("verdict is not a dict")
    sv = v.get("schema")
    if sv != SCHEMA_VERSION:
        die(f"unknown schema version {sv}; expected {SCHEMA_VERSION}")
    missing = VERDICT_FIELDS - v.keys()
    if missing:
        die(f"verdict missing fields: {sorted(missing)}")
    unknown = v.keys() - VERDICT_FIELDS
    if unknown:
        die(f"verdict unknown fields: {sorted(unknown)}")
    if v["phase"] not in PHASES:
        die(f"invalid phase: {v['phase']}")
    if v["kind"] not in KINDS:
        die(f"invalid kind: {v['kind']}")
    if v["kind"] == "no-gate" and not v.get("reason"):
        die("no-gate verdict requires a non-empty reason")
    runner_mode = v["runner"].split("@")[-1]
    if runner_mode not in RUNNER_MODES:
        die(f"invalid runner mode: {runner_mode!r}")
    if not isinstance(v.get("gates"), list):
        die("gates must be a list")
    for g in v["gates"]:
        if not isinstance(g, dict):
            die("each gate entry must be a dict")
        missing = GATE_FIELDS - g.keys()
        if missing:
            die(f"gate entry missing fields: {sorted(missing)}")
        if not isinstance(g.get("exit"), int) or g["exit"] < -2:
            die(f"gate exit must be int >= -2, got {g.get('exit')!r}")
        if not isinstance(g.get("duration_s"), (int, float)):
            die(f"gate duration_s must be numeric, got {g.get('duration_s')!r}")
    if not isinstance(v.get("pass"), bool):
        die("pass must be a bool")
    if not isinstance(v.get("scope"), dict):
        die("scope must be a dict")


def append_verdict(task_id, verdict):
    """Validate and append a verdict to the task's JSONL file (I4, D5)."""
    _verify_schema(verdict)
    ensure_verdicts_dir()
    path = task_path(task_id)
    if path.exists():
        with open(path) as f:
            for i, line in enumerate(f, 1):
                line = line.strip()
                if not line:
                    continue
                try:
                    existing = json.loads(line)
                except json.JSONDecodeError as e:
                    die(f"{path}:{i}: invalid JSON — {e}")
                _verify_schema(existing)
    with open(path, "a") as f:
        f.write(json.dumps(verdict, sort_keys=True) + "\n")


def read_verdicts(task_id):
    """Read all verdicts for a task. Dies on unparseable lines (I4)."""
    path = task_path(task_id)
    if not path.exists():
        return []
    verdicts = []
    with open(path) as f:
        for i, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                v = json.loads(line)
            except json.JSONDecodeError as e:
                die(f"{path}:{i}: invalid JSON — {e}")
            _verify_schema(v)
            verdicts.append(v)
    return verdicts


def extract_gates(brief_path):
    """Extract gate commands from a markdown brief's Gate section (I3).

    Finds **Gate:** or - **Gate:** marker, then collects from the section
    body: backtick-quoted inline commands and indented code-block lines.

    Returns a list of command strings (deduplicated, order-preserved).
    """
    text = Path(brief_path).read_text()
    lines = text.split("\n")

    gate_start = None
    for i, line in enumerate(lines):
        if GATE_HEADING.match(line):
            gate_start = i
            break

    if gate_start is None:
        return []

    body_lines = []
    for line in lines[gate_start + 1:]:
        if SECTION_LABEL.match(line):
            break
        body_lines.append(line)

    body = "\n".join(body_lines)
    commands = []

    for m in re.finditer(r"`([^`]+)`", body):
        cmd = m.group(1).strip()
        if cmd:
            commands.append(cmd)

    for line in body_lines:
        if re.match(r"^ {4,}", line) or line.startswith("\t"):
            cmd = line.strip()
            if cmd:
                commands.append(cmd)

    seen = set()
    unique = []
    for cmd in commands:
        if cmd not in seen:
            seen.add(cmd)
            unique.append(cmd)
    return unique


def run_gate(cmd, timeout=GATE_TIMEOUT_S):
    """Run a single gate command via subprocess.

    Returns (exit_code, tail_text, duration_s).
    """
    start = time.time()
    try:
        r = subprocess.run(
            cmd, shell=True, capture_output=True, text=True, timeout=timeout,
        )
        duration = time.time() - start
        output = (r.stdout or "") + (r.stderr or "")
        lines = output.rstrip("\n").split("\n")
        tail = "\n".join(lines[-TAIL_LINES:])
        return r.returncode, tail, round(duration, 1)
    except subprocess.TimeoutExpired:
        duration = time.time() - start
        return -1, f"TIMEOUT after {duration:.1f}s", round(duration, 1)
    except Exception as e:
        duration = time.time() - start
        return -2, str(e), round(duration, 1)


def cmd_per_lane(args):
    """Run gate commands from the brief and append a per-lane verdict."""
    brief_path = Path(args.brief)
    if not brief_path.exists():
        die(f"brief not found: {brief_path}")

    gates = extract_gates(str(brief_path))
    if not gates:
        die(f"I3: no gate commands found in Gate section of {brief_path}")

    results = []
    all_pass = True
    for cmd in gates:
        exit_code, tail, duration = run_gate(cmd)
        passed = exit_code == 0
        if not passed:
            all_pass = False
        results.append({
            "cmd": cmd,
            "exit": exit_code,
            "duration_s": duration,
            "tail": tail,
        })

    verdict = {
        "schema": SCHEMA_VERSION,
        "task": args.task,
        "phase": "per-lane",
        "brief": str(brief_path),
        "branch": args.branch or "unknown",
        "commit": args.commit or None,
        "gates": results,
        "scope": {"files_changed": [], "in_scope": True},
        "pass": all_pass,
        "kind": "gate",
        "reason": None,
        "runner": "gate_runner.py@lead",
        "ts": datetime.now(timezone.utc).isoformat(),
    }

    append_verdict(args.task, verdict)

    total = len(results)
    passed = sum(1 for r in results if r["exit"] == 0)
    print(f"Gates: {passed}/{total} passed ({total - passed} failed)")
    for r in results:
        status = "PASS" if r["exit"] == 0 else f"FAIL (exit {r['exit']})"
        print(f"  [{status}] {r['cmd']} ({r['duration_s']}s)")

    sys.exit(0 if all_pass else 1)


def cmd_no_gate(args):
    """Append a no-gate verdict (explicit bypass)."""
    if not args.reason:
        die("no-gate requires --reason")

    verdict = {
        "schema": SCHEMA_VERSION,
        "task": args.task,
        "phase": "per-lane",
        "brief": "",
        "branch": "unknown",
        "commit": None,
        "gates": [],
        "scope": {"files_changed": [], "in_scope": True},
        "pass": True,
        "kind": "no-gate",
        "reason": args.reason,
        "runner": "gate_runner.py@lead",
        "ts": datetime.now(timezone.utc).isoformat(),
    }
    append_verdict(args.task, verdict)
    print(f"no-gate verdict appended for task {args.task}: {args.reason}")
    sys.exit(0)


def cmd_show(args):
    """Print verdicts for a task from the trail."""
    verdicts = read_verdicts(args.task)
    if not verdicts:
        print(f"No verdicts for task {args.task}")
        sys.exit(0)

    for v in verdicts:
        status = "PASS" if v["pass"] else "FAIL"
        kind = v["kind"]
        phase = v["phase"]
        n_gates = len(v.get("gates", []))
        reason = f"  Reason: {v['reason']}" if v.get("reason") else ""
        print(f"[{status}] {v['task']} {phase} ({kind}, {n_gates} gates)")
        if reason:
            print(reason)
        for g in v.get("gates", []):
            gstatus = "PASS" if g["exit"] == 0 else f"FAIL (exit {g['exit']})"
            print(f"  {gstatus} {g['cmd']} ({g['duration_s']}s)")
    sys.exit(0)


def main():
    parser = argparse.ArgumentParser(
        description="Gate Runtime — verdicts the machine writes",
    )
    sub = parser.add_subparsers(dest="subcommand", required=True)

    pl = sub.add_parser("per-lane")
    pl.add_argument("--task", required=True, help="Task ID (BUG-xxx)")
    pl.add_argument("--brief", required=True, help="Path to brief markdown file")
    pl.add_argument("--branch", default=None, help="Branch name")
    pl.add_argument("--commit", default=None, help="Commit SHA")
    pl.set_defaults(func=cmd_per_lane)

    ng = sub.add_parser("no-gate")
    ng.add_argument("--task", required=True, help="Task ID (BUG-xxx)")
    ng.add_argument("--reason", required=True, help="Reason for bypass")
    ng.set_defaults(func=cmd_no_gate)

    sh = sub.add_parser("show")
    sh.add_argument("--task", required=True, help="Task ID (BUG-xxx)")
    sh.set_defaults(func=cmd_show)

    args = parser.parse_args()
    ensure_verdicts_dir()
    args.func(args)


if __name__ == "__main__":
    main()
