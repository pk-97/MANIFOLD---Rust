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
import importlib.util
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

SCHEMA_VERSION = 1
GATE_TIMEOUT_S = 300
TAIL_LINES = 20

REPO = Path(__file__).resolve().parent.parent
VERDICTS_DIR = Path(
    os.environ.get("GATE_RUNNER_VERDICTS_DIR")
    or REPO / ".claude" / "orchestration" / "verdicts"
)

# Pre-wave checks: the main checkout is the canonical repo root
# (worktrees are isolated but goldens/wave-base checks need the main checkout).
MAIN_CHECKOUT = Path("/Users/peterkiemann/MANIFOLD - Rust")
DEFAULT_LITELLM_URL = "http://127.0.0.1:4000/health/liveliness"

# Slot labels from the naming guard (single source of truth) — the guard
# derives them from the session env, so gate_runner sees the same map the
# spawn-time hook will enforce.
_GUARD_PATH = REPO / ".claude" / "hooks" / "agent-teammate-naming-guard.py"
_guard_spec = importlib.util.spec_from_file_location("_naming_guard", str(_GUARD_PATH))
_guard_mod = importlib.util.module_from_spec(_guard_spec)
_guard_spec.loader.exec_module(_guard_mod)
VALID_SLOTS = sorted({label for _b, label in _guard_mod.slot_map().values()})

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
    body: backtick-quoted inline commands, indented code-block lines, and
    triple-backtick fenced code blocks (BUG-aayj item 2).

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

    # Backtick-quoted inline commands (newline-delimited: triple-backtick
    # fenced blocks are handled separately; this pattern must not match
    # across lines to avoid capturing fence body as inline code).
    for m in re.finditer(r"`([^`\n]+)`", body):
        cmd = m.group(1).strip()
        if cmd:
            commands.append(cmd)

    # Indented code blocks (4+ spaces or tab)
    for line in body_lines:
        if re.match(r"^ {4,}", line) or line.startswith("\t"):
            cmd = line.strip()
            if cmd:
                commands.append(cmd)

    # Fenced code blocks (triple backticks)
    in_fence = False
    for line in body_lines:
        stripped = line.strip()
        if stripped.startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence and stripped:
            commands.append(stripped)

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


def scope_from_git(commit, brief_text):
    """Populate verdict scope from the real diff (BUG-aayj).

    files_changed = git diff --name-only merge-base(origin/main, commit)..commit.
    in_scope: every changed file path must appear in the brief text; a brief
    naming no repo paths has nothing to check against (in_scope=True). Any
    git failure = empty scope, never a crash (scope is evidence, not a gate).
    """
    empty = {"files_changed": [], "in_scope": True}
    if not commit:
        return empty
    try:
        mb = subprocess.run(
            ["git", "merge-base", "origin/main", commit],
            capture_output=True, text=True, timeout=15, cwd=str(REPO),
        )
        if mb.returncode != 0:
            return empty
        base = mb.stdout.strip()
        d = subprocess.run(
            ["git", "diff", "--name-only", f"{base}..{commit}"],
            capture_output=True, text=True, timeout=15, cwd=str(REPO),
        )
        if d.returncode != 0:
            return empty
        changed = [l.strip() for l in d.stdout.splitlines() if l.strip()]
        named_paths = re.findall(r"[\w.-]+(?:/[\w.-]+)+", brief_text)
        if not named_paths:
            return {"files_changed": changed, "in_scope": True}
        in_scope = all(any(f in n or n in f for n in named_paths) for f in changed)
        return {"files_changed": changed, "in_scope": in_scope}
    except Exception:
        return empty


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

    # Scope is evidence for the reviewer (SCOPE_CHECK is the dispatcher's
    # opcode, IR §2), never a pass/fail input here.
    scope = scope_from_git(args.commit, brief_path.read_text())

    verdict = {
        "schema": SCHEMA_VERSION,
        "task": args.task,
        "phase": "per-lane",
        "brief": str(brief_path),
        "branch": args.branch or "unknown",
        "commit": args.commit or None,
        "gates": results,
        "scope": scope,
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


# ---------------------------------------------------------------------------
# Pre-wave checks (P2)
# ---------------------------------------------------------------------------


def _print_check(status, name, detail):
    """Print a single check line: [PASS|FAIL|WARN] name — detail."""
    print(f"  [{status}] {name} — {detail}")


def _check_seat_drift():
    """Check a: seat_tool show — FAIL if any slot has DRIFT or NO."""
    cmd_label = "seat drift"
    start = time.time()
    tail_parts = []
    try:
        r = subprocess.run(
            ["python3", str(REPO / "scripts/seat_tool.py"), "show"],
            capture_output=True, text=True, timeout=30,
        )
        duration = round(time.time() - start, 1)
        lines = r.stdout.strip().split("\n")
        failed = False
        for line in lines:
            if not line.strip() or line.strip().startswith("slot"):
                continue  # skip header
            if "<- DRIFT" in line:
                failed = True
                tail_parts.append(f"DRIFT: {line.strip()}")
                continue
            parts = line.split()
            if len(parts) >= 5 and parts[4] == "NO":
                failed = True
                tail_parts.append(f"UNSERVED: {line.strip()}")
        tail = "; ".join(tail_parts) if tail_parts else "all slots aligned"
        exit_code = 1 if failed else 0
        status = "FAIL" if failed else "PASS"
        _print_check(status, cmd_label, tail)
        return {"cmd": cmd_label, "exit": exit_code, "duration_s": duration, "tail": tail}
    except Exception as e:
        duration = round(time.time() - start, 1)
        _print_check("FAIL", cmd_label, str(e))
        return {"cmd": cmd_label, "exit": 1, "duration_s": duration, "tail": str(e)}


def _check_litellm(litellm_url):
    """Check b: litellm /health/liveliness — FAIL unless 200."""
    cmd_label = "litellm liveliness"
    start = time.time()
    try:
        req = urllib.request.Request(litellm_url)
        resp = urllib.request.urlopen(req, timeout=10)
        status = resp.getcode()
        duration = round(time.time() - start, 1)
        if status == 200:
            _print_check("PASS", cmd_label, f"HTTP {status}")
            return {"cmd": cmd_label, "exit": 0, "duration_s": duration, "tail": f"HTTP {status}"}
        else:
            _print_check("FAIL", cmd_label, f"HTTP {status}")
            return {"cmd": cmd_label, "exit": 1, "duration_s": duration, "tail": f"HTTP {status}"}
    except Exception as e:
        duration = round(time.time() - start, 1)
        err = str(e)
        _print_check("FAIL", cmd_label, err)
        return {"cmd": cmd_label, "exit": 1, "duration_s": duration, "tail": err}


def _check_quota():
    """Check c: kimi usage — WARN-only, never FAIL."""
    cmd_label = "quota"
    start = time.time()
    try:
        key_r = subprocess.run(
            ["cc-fleet", "keyget", "kimi-upstream"],
            capture_output=True, text=True, timeout=10,
        )
        if key_r.returncode != 0:
            tail = f"keyget failed: {key_r.stderr.strip() or 'no key'}"
            duration = round(time.time() - start, 1)
            _print_check("WARN", cmd_label, tail)
            return {"cmd": cmd_label, "exit": 0, "duration_s": duration, "tail": tail}

        token = key_r.stdout.strip()
        req = urllib.request.Request(
            "https://api.kimi.com/coding/v1/usages",
            headers={"Authorization": f"Bearer {token}"},
        )
        resp = urllib.request.urlopen(req, timeout=15)
        data = json.loads(resp.read().decode())
        duration = round(time.time() - start, 1)

        # 5h (300-min) window — API returns strings, convert to int
        pct_5h = None
        for lim in data.get("limits", []):
            if lim.get("window", {}).get("duration") == 300:
                detail = lim.get("detail", {}) or {}
                limit = int(detail.get("limit") or 0)
                remaining = int(detail.get("remaining") or 0)
                if limit > 0:
                    pct_5h = (limit - remaining) * 100 // limit
                break

        # weekly quota
        pct_7d = None
        usage = data.get("usage") or {}
        limit7 = int(usage.get("limit") or 0)
        remaining7 = int(usage.get("remaining") or 0)
        if limit7 > 0:
            pct_7d = (limit7 - remaining7) * 100 // limit7

        parts = []
        if pct_5h is not None:
            parts.append(f"5h {pct_5h}%")
        if pct_7d is not None:
            parts.append(f"7d {pct_7d}%")
        tail = ", ".join(parts) if parts else "no quota data"
        _print_check("WARN", cmd_label, tail)
        return {"cmd": cmd_label, "exit": 0, "duration_s": duration, "tail": tail}
    except Exception as e:
        duration = round(time.time() - start, 1)
        _print_check("WARN", cmd_label, str(e))
        return {"cmd": cmd_label, "exit": 0, "duration_s": duration, "tail": str(e)}


def _check_goldens():
    """Check d: git status --porcelain on goldens dir — FAIL if dirty."""
    cmd_label = "goldens clean"
    start = time.time()
    goldens_path = "tests/fixtures/gltf/goldens/"
    try:
        r = subprocess.run(
            ["git", "status", "--porcelain", goldens_path],
            capture_output=True, text=True, timeout=15,
            cwd=str(MAIN_CHECKOUT),
        )
        duration = round(time.time() - start, 1)
        dirty = [l.strip() for l in r.stdout.strip().split("\n") if l.strip()]
        if not dirty:
            _print_check("PASS", cmd_label, "clean")
            return {"cmd": cmd_label, "exit": 0, "duration_s": duration, "tail": "clean"}
        else:
            tail = ", ".join(dirty[:5])
            _print_check("FAIL", cmd_label, f"dirty: {tail}")
            return {"cmd": cmd_label, "exit": 1, "duration_s": duration, "tail": tail}
    except Exception as e:
        duration = round(time.time() - start, 1)
        _print_check("FAIL", cmd_label, str(e))
        return {"cmd": cmd_label, "exit": 1, "duration_s": duration, "tail": str(e)}


def _check_wave_base(wave_base):
    """Check e: wave base sha is ancestor of origin/main — FAIL if not."""
    cmd_label = "wave base merged"
    start = time.time()
    if not wave_base:
        duration = round(time.time() - start, 1)
        _print_check("WARN", cmd_label, "--base omitted, skipping")
        return {"cmd": cmd_label, "exit": 0, "duration_s": duration, "tail": "skipped (--base omitted)"}
    try:
        r = subprocess.run(
            ["git", "merge-base", "--is-ancestor", wave_base, "origin/main"],
            capture_output=True, text=True, timeout=15,
            cwd=str(MAIN_CHECKOUT),
        )
        duration = round(time.time() - start, 1)
        if r.returncode == 0:
            _print_check("PASS", cmd_label, f"{wave_base[:12]} is ancestor of origin/main")
            return {"cmd": cmd_label, "exit": 0, "duration_s": duration, "tail": f"{wave_base[:12]} ancestor of origin/main"}
        else:
            _print_check("FAIL", cmd_label, f"{wave_base[:12]} is NOT ancestor of origin/main")
            return {"cmd": cmd_label, "exit": 1, "duration_s": duration, "tail": f"{wave_base[:12]} NOT ancestor of origin/main"}
    except Exception as e:
        duration = round(time.time() - start, 1)
        _print_check("FAIL", cmd_label, str(e))
        return {"cmd": cmd_label, "exit": 1, "duration_s": duration, "tail": str(e)}


def cmd_pre_wave(args):
    """Run the five P2 pre-wave checks and append a verdict."""
    litellm_url = os.environ.get("LITELLM_URL") or args.litellm_url or DEFAULT_LITELLM_URL

    print("=== pre-wave preflight ===")
    checks = [
        _check_seat_drift(),
        _check_litellm(litellm_url),
        _check_quota(),
        _check_goldens(),
        _check_wave_base(args.base),
    ]

    all_pass = all(g["exit"] == 0 for g in checks)
    verdict = {
        "schema": SCHEMA_VERSION,
        "task": "pre-wave",
        "phase": "pre-wave",
        "brief": "",
        "branch": "unknown",
        "commit": None,
        "gates": checks,
        "scope": {"files_changed": [], "in_scope": True},
        "pass": all_pass,
        "kind": "gate",
        "reason": None,
        "runner": "gate_runner.py@preflight",
        "ts": datetime.now(timezone.utc).isoformat(),
    }
    append_verdict("pre-wave", verdict)

    total = len(checks)
    passed = sum(1 for g in checks if g["exit"] == 0)
    print(f"pre-wave: {passed}/{total} checks passed")
    sys.exit(0 if all_pass else 1)


# ---------------------------------------------------------------------------
# Pre-dispatch checks (P3)
# ---------------------------------------------------------------------------

ANCHOR_RE = re.compile(r"([\w./-]+\.[a-zA-Z]+):(\d+)")
SLOT_SCAN_RE = re.compile(r"\b(flash|glm47|glm52|k3|pro)-")


def _check_anchors(text, repo_root, brief_dir):
    """Check a: every file:line anchor resolves (file exists, line in range).

    `file (symbol)` form is allowed to pass unchecked.
    Returns (exit_code, detail).
    """
    fails = []
    for m in ANCHOR_RE.finditer(text):
        path_str, line_str = m.group(1), m.group(2)
        line_num = int(line_str)

        # Skip URL-looking things
        if path_str.startswith(("http://", "https://", "ftp://")):
            continue

        found = False
        for base in [repo_root, brief_dir]:
            candidate = base / path_str
            try:
                if candidate.is_file():
                    file_line_count = len(candidate.read_text().splitlines())
                    if 1 <= line_num <= file_line_count:
                        found = True
                        break
                    else:
                        fails.append(
                            f"{path_str}:{line_num} (line {line_num} out of range, "
                            f"file has {file_line_count} lines)"
                        )
                        found = True  # file found but line bad
                        break
            except Exception:
                pass

        if not found:
            fails.append(f"{path_str}:{line_num} (file not found)")

    for f in fails:
        print(f"  [FAIL] anchor: {f}")
    if not fails:
        print("  [PASS] anchors — all file:line references resolve")
    return (1 if fails else 0, "; ".join(fails) if fails else "all resolve")


def _check_gates_parse(brief_path):
    """Check b: gate commands from the brief shell-parse (bash -n).

    I3: a brief with no Gate section FAILs. Fenced blocks supported.
    Returns (exit_code, detail).
    """
    gates = extract_gates(str(brief_path))
    if not gates:
        print("  [FAIL] gates — no Gate section found in brief (I3)")
        return (1, "no Gate section (I3)")

    parse_fails = []
    for cmd in gates:
        r = subprocess.run(
            ["bash", "-n"], input=cmd,
            capture_output=True, text=True, timeout=5,
        )
        if r.returncode != 0:
            stderr = r.stderr.strip()
            parse_fails.append(f"command {cmd!r}: {stderr}")

    if parse_fails:
        for f in parse_fails:
            print(f"  [FAIL] gate parse: {f}")
        return (1, "; ".join(parse_fails[:5]))
    else:
        plural = "s" if len(gates) != 1 else ""
        print(f"  [PASS] gates — {len(gates)} gate{plural} shell-parse successfully")
        return (0, f"{len(gates)} gates parse")


def _check_slots(text):
    """Check c: every named lane slot prefix is a valid slot label.

    Scans for tokens matching slot-prefix pattern, validates each against
    VALID_SLOTS (from agent-teammate-naming-guard.py).
    Returns (exit_code, detail).
    """
    fails = []
    for m in SLOT_SCAN_RE.finditer(text):
        slot = m.group(1)
        if slot not in VALID_SLOTS:
            fails.append(
                f"slot {slot!r} (from {m.group(0)!r}) is not in "
                f"VALID_SLOTS={VALID_SLOTS}"
            )

    if fails:
        for f in fails:
            print(f"  [FAIL] slot: {f}")
        return (1, "; ".join(fails))
    else:
        print("  [PASS] slots — all lane slot names are valid")
        return (0, "all slots valid")


def _check_bead(text):
    """Check d: brief must contain a task id matching BUG-\\w+."""
    m = re.search(r"BUG-\w+", text)
    if m:
        print(f"  [PASS] bead — task ID {m.group(0)!r} found")
        return (0, f"bug id {m.group(0)} present")
    else:
        print("  [FAIL] bead — no BUG-... task ID in brief")
        return (1, "missing bug id")


def cmd_pre_dispatch(args):
    """Run the four P3 pre-dispatch brief lint checks.

    Appends one schema-1 verdict to pre-dispatch.jsonl. Exits 0 iff no FAIL.
    """
    brief_path = Path(args.brief)
    if not brief_path.exists():
        die(f"brief not found: {brief_path}")

    text = brief_path.read_text()
    repo_root = Path(__file__).resolve().parent.parent
    brief_dir = brief_path.resolve().parent

    checks = [
        ("anchors", *_check_anchors(text, repo_root, brief_dir)),
        ("gates_parse", *_check_gates_parse(brief_path)),
        ("slots", *_check_slots(text)),
        ("bead", *_check_bead(text)),
    ]

    verdict_gates = [
        {"cmd": f"{name} check", "exit": exit_code, "duration_s": 0.0, "tail": detail}
        for name, exit_code, detail in checks
    ]
    all_pass = all(exit_code == 0 for _, exit_code, _ in checks)

    bead_match = re.search(r"BUG-\w+", text)
    task_id = bead_match.group(0) if bead_match else "pre-dispatch"

    verdict = {
        "schema": SCHEMA_VERSION,
        "task": task_id,
        "phase": "pre-dispatch",
        "brief": str(brief_path),
        "branch": "unknown",
        "commit": None,
        "gates": verdict_gates,
        "scope": {"files_changed": [], "in_scope": True},
        "pass": all_pass,
        "kind": "gate",
        "reason": None,
        "runner": "gate_runner.py@lint",
        "ts": datetime.now(timezone.utc).isoformat(),
    }

    ensure_verdicts_dir()
    verdict_path = VERDICTS_DIR / "pre-dispatch.jsonl"
    with open(verdict_path, "a") as f:
        f.write(json.dumps(verdict, sort_keys=True) + "\n")

    n_passed = sum(1 for _, ec, _ in checks if ec == 0)
    total = len(checks)
    print(f"\npre-dispatch lint: {n_passed}/{total} checks passed")
    sys.exit(0 if all_pass else 1)


# ---------------------------------------------------------------------------
# Report subcommand (P4 / D7)
# ---------------------------------------------------------------------------

_DECISIONS_ENTRY_RE = re.compile(r"^- \*\*[A-Z0-9]+-D?\d+[\s\)\*]", re.MULTILINE)
_DECISIONS_PATH = MAIN_CHECKOUT / ".claude" / "orchestration" / "decisions.md"
_BEADS_CMD = ["bd", "list", "--status", "closed", "--json", "--flat"]


def _resolve_since(since):
    """Convert --since to a datetime, or None on failure.

    Tries git-ref resolution first (rev-parse), then ISO date parsing."""
    if not since:
        return None
    try:
        r = subprocess.run(
            ["git", "-C", str(MAIN_CHECKOUT), "log", "-1", "--format=%cI", since],
            capture_output=True, text=True, timeout=10,
        )
        if r.returncode == 0 and r.stdout.strip():
            return datetime.fromisoformat(r.stdout.strip())
    except Exception:
        pass
    try:
        return datetime.fromisoformat(since)
    except (ValueError, TypeError):
        die(f"cannot resolve --since={since!r}: not a git ref or ISO date")


def _count_verdicts_by_phase(since_dt):
    """Read all verdicts from the trail and count pass/fail by phase.

    Returns dict: {phase: {"pass": N, "fail": M, "total": T}}."""
    if not VERDICTS_DIR.is_dir():
        return {}
    counts = {}
    for p in sorted(VERDICTS_DIR.glob("*.jsonl")):
        with open(p) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    v = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if since_dt:
                    try:
                        v_ts = datetime.fromisoformat(v.get("ts", ""))
                        if v_ts < since_dt:
                            continue
                    except (ValueError, TypeError):
                        pass
                phase = v.get("phase", "unknown")
                passed = v.get("pass", False)
                if phase not in counts:
                    counts[phase] = {"pass": 0, "fail": 0, "total": 0}
                counts[phase]["total"] += 1
                if passed:
                    counts[phase]["pass"] += 1
                else:
                    counts[phase]["fail"] += 1
    return counts


def _count_beads_closed(since_dt):
    """Count closed beads since the given datetime.

    Returns (count, error_or_None)."""
    try:
        r = subprocess.run(_BEADS_CMD, capture_output=True, text=True, timeout=15)
        if r.returncode != 0:
            return 0, f"bd exited {r.returncode}"
        try:
            beads = json.loads(r.stdout)
        except json.JSONDecodeError:
            return 0, "bd output not valid JSON"
        if since_dt is None:
            return len(beads), None
        count = 0
        for b in beads:
            ca = b.get("closed_at")
            if not ca:
                continue
            try:
                closed_dt = datetime.fromisoformat(
                    ca.replace("Z", "+00:00")).replace(tzinfo=None)
                if closed_dt >= since_dt:
                    count += 1
            except (ValueError, TypeError, AttributeError):
                pass
        return count, None
    except Exception as e:
        return 0, str(e)


def _count_decisions_entries(since_dt):
    """Count decisions.md entries in the main checkout.

    Counts lines matching the entry pattern (^- **<prefix>-<N>**)."""
    path = _DECISIONS_PATH
    if not path.exists():
        return 0, None
    text = path.read_text()
    total = len(_DECISIONS_ENTRY_RE.findall(text))

    if since_dt is None:
        return total, None

    # Count added entries since the ref using git diff
    try:
        since_iso = since_dt.strftime("%Y-%m-%dT%H:%M:%S%z")
        # Use git log to find added entry lines since the ref
        r = subprocess.run(
            ["git", "-C", str(MAIN_CHECKOUT), "diff", "--unified=0",
             f"@{since_iso}", "--",
             str(_DECISIONS_PATH.relative_to(MAIN_CHECKOUT))],
            capture_output=True, text=True, timeout=15,
        )
        added = 0
        if r.returncode == 0:
            for line in r.stdout.split("\n"):
                if line.startswith("+") and not line.startswith("+++") \
                        and _DECISIONS_ENTRY_RE.match(line[1:]):
                    added += 1
        return added if added else total, None
    except Exception:
        return total, None


def cmd_report(args):
    """Print a wave-activity report since the given ref/date.

    Counts trail verdicts, beads closes, and decisions.md entries.
    Read-only — no verdict appended (D7)."""
    since_dt = _resolve_since(args.since)

    verdict_counts = _count_verdicts_by_phase(since_dt)
    n_beads, beads_err = _count_beads_closed(since_dt)
    n_decisions, dec_err = _count_decisions_entries(since_dt)

    since_str = since_dt.strftime("%Y-%m-%d %H:%M:%S %z") if since_dt else "all time"
    print(f"=== Wave report since {since_str} ===\n")

    print("Verdicts:")
    if verdict_counts:
        print(f"  {'Phase':<20} {'Pass':>6} {'Fail':>6} {'Total':>6}")
        print(f"  {'-'*20} {'-'*6} {'-'*6} {'-'*6}")
        total_pass = total_fail = 0
        for phase in sorted(verdict_counts.keys()):
            pc = verdict_counts[phase]
            print(f"  {phase:<20} {pc['pass']:>6} {pc['fail']:>6} {pc['total']:>6}")
            total_pass += pc["pass"]
            total_fail += pc["fail"]
        print(f"  {'-'*20} {'-'*6} {'-'*6} {'-'*6}")
        print(f"  {'TOTAL':<20} {total_pass:>6} {total_fail:>6} {total_pass + total_fail:>6}")
    else:
        print("  (no verdicts)")

    beads_str = str(n_beads)
    if beads_err:
        beads_str += f" (error: {beads_err})"
    print(f"Beads closed: {beads_str}")

    dec_str = str(n_decisions)
    if dec_err:
        dec_str += f" (error: {dec_err})"
    print(f"Decisions.md entries: {dec_str}")

    print()
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

    pw = sub.add_parser("pre-wave", help="Run pre-wave preflight checks (P2)")
    pw.add_argument("--litellm-url", default=None, help="Override litellm health URL (default: env LITELLM_URL or built-in)")
    pw.add_argument("--base", default=None, help="Wave base SHA to verify is ancestor of origin/main")
    pw.set_defaults(func=cmd_pre_wave)

    pd = sub.add_parser("pre-dispatch", help="Run pre-dispatch brief lint (P3)")
    pd.add_argument("--brief", required=True, help="Path to brief markdown file")
    pd.set_defaults(func=cmd_pre_dispatch)

    rp = sub.add_parser("report", help="Wave-activity report (P4 / D7)")
    rp.add_argument("--since", default=None,
                    help="Git ref or ISO date to report since (default: all time)")
    rp.set_defaults(func=cmd_report)

    args = parser.parse_args()
    ensure_verdicts_dir()
    args.func(args)


if __name__ == "__main__":
    main()
