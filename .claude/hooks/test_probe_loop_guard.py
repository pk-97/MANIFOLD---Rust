#!/usr/bin/env python3
"""Tests for probe-loop-guard.py — run: python3 test_probe_loop_guard.py

Covers the BUG-0c28 (read-only git plumbing exempt) exemption and the
BUG-yo9m (lead probe loop must trip warn/deny) replay: 15 probe actions
with a STALE unlock file present — the 2026-07-28 silent-disarm pattern —
must warn at 3 and deny from 6 on.
"""
import json
import os
import subprocess
import sys
import time
import uuid
from pathlib import Path

HOOK = Path(__file__).with_name("probe-loop-guard.py")
REVIEW_FILE = "/tmp/manifold_seam_review.md"


def run(payload):
    p = subprocess.run(
        [sys.executable, str(HOOK)], input=json.dumps(payload),
        capture_output=True, text=True,
    )
    assert p.returncode == 0, p.stderr
    return json.loads(p.stdout) if p.stdout.strip() else None


def bash(cmd, session, **extra):
    return {"tool_name": "Bash", "tool_input": {"command": cmd},
            "session_id": session, **extra}


def edit(path, session):
    return {"tool_name": "Edit", "tool_input": {"file_path": path},
            "session_id": session}


def kind(out):
    if out is None:
        return "silent"
    h = out["hookSpecificOutput"]
    if h.get("permissionDecision") == "deny":
        return "deny"
    if "additionalContext" in h:
        return "warn"
    return "other"


def fresh_session():
    s = f"test-{uuid.uuid4().hex[:12]}"
    cp = f"/tmp/manifold_probe_loop_{s}.json"
    if os.path.exists(cp):
        os.remove(cp)
    return s


def counter(session):
    cp = f"/tmp/manifold_probe_loop_{session}.json"
    if not os.path.exists(cp):
        return 0
    return json.load(open(cp)).get("count", 0)


saved_review = None
if os.path.exists(REVIEW_FILE):
    saved_review = open(REVIEW_FILE, "rb").read()
    os.remove(REVIEW_FILE)

failures = []


def check(name, cond, detail=""):
    if cond:
        print(f"  PASS {name}")
    else:
        print(f"  FAIL {name} {detail}")
        failures.append(name)


# --- BUG-0c28: read-only git plumbing never counts, even with probe
# markers in branch names / arguments ---
s = fresh_session()
plumbing = [
    'for b in lane/render-import-fix lane/x; do git merge-base --is-ancestor "$b" origin/main; done',
    'git rev-parse --short lane/gpu-proofs-gate',
    'git -C "/Users/x/MANIFOLD - Rust" log --oneline --grep=gpu-proofs',
    'git branch --show-current',
    'git -C /tmp/wt merge-base --is-ancestor lane/gpu_proofs_gate origin/main',
]
for i in range(2):  # 10 calls total — far past DENY_AT if miscounted
    for c in plumbing:
        out = run(bash(c, s))
        check(f"plumbing silent: {c[:40]}…", kind(out) == "silent", f"got {kind(out)}")
check("plumbing never counted", counter(s) == 0, f"count={counter(s)}")

# --- real probe commands still count: warn at 3 ---
s = fresh_session()
probe_cmd = "cargo test -p manifold-renderer --features gpu-proofs"
check("probe 1 silent", kind(run(bash(probe_cmd, s))) == "silent")
check("probe 2 silent", kind(run(bash(probe_cmd, s))) == "silent")
check("probe 3 warns", kind(run(bash(probe_cmd, s))) == "warn")

# --- compound: plumbing prefix does not shield a probe suffix ---
s = fresh_session()
run(bash("git log --oneline && cargo test --features gpu-proofs", s))
check("plumbing+probe compound counts", counter(s) == 1, f"count={counter(s)}")

# --- BUG-q329: textual mentions are not probes ---
s = fresh_session()
mentions = [
    "bd close BUG-yo9m -r 'verified: 15-probe replay with render-import runs'",
    'git commit -m "gpu_proofs_gate.py: full-suite gate (BUG-gtir)" -- CLAUDE.md',
    "rg -n gpu-proofs docs/ scripts/",
    "git merge --no-ff lane/gpu-proofs-gate",
    "python3 -m py_compile scripts/foo.py && echo gpu-proofs mentioned",
    "ls lane/render-import-fix/",
]
for i in range(2):  # 12 calls — far past DENY_AT if miscounted
    for c in mentions:
        out = run(bash(c, s))
        check(f"mention silent: {c[:40]}…", kind(out) == "silent", f"got {kind(out)}")
check("mentions never counted", counter(s) == 0, f"count={counter(s)}")

# --- executions still count: wrapper run and direct binary run ---
s = fresh_session()
run(bash('python3 "scripts/gpu_proofs_gate.py" --manifest-path /tmp/wt/Cargo.toml', s))
run(bash("target/release/render-import fixtures/a.glb /tmp/out.png", s))
check("wrapper + binary runs count", counter(s) == 2, f"count={counter(s)}")

# --- for-loop BODY running a probe still counts (header is data, body is not) ---
s = fresh_session()
run(bash("for f in a.glb b.glb; do cargo run --bin render-import -- $f; done", s))
check("for-body probe counts", counter(s) == 1, f"count={counter(s)}")

# --- worker seats exempt ---
s = fresh_session()
out = run(bash(probe_cmd, s, agent_id="lane-1"))
check("worker seat silent", kind(out) == "silent" and counter(s) == 0)

# --- BUG-yo9m replay: 15 probe actions (kernel-edit + render-import mix),
# STALE review file present the whole time. Expect warn at 3, deny 6..15. ---
s = fresh_session()
with open(REVIEW_FILE, "w") as f:
    f.write("stale evidence table " * 20)  # >=200 chars, mtime BEFORE any probe
time.sleep(0.05)
results = []
for i in range(15):
    if i % 2 == 0:
        p = edit("/Users/x/wt/slot-8/crates/manifold-gpu/src/metal/render_scene.rs", s)
    else:
        p = bash("cargo run --bin render-import -- fixtures/lowe.glb /tmp/out.png", s)
    results.append(kind(run(p)))
    time.sleep(0.01)
check("replay: 1-2 silent", results[0] == results[1] == "silent", results[:2])
check("replay: warns at 3", results[2] == "warn", results[2])
check("replay: 4-5 silent", results[3] == results[4] == "silent", results[3:5])
check("replay: denies 6..15", all(r == "deny" for r in results[5:]), results[5:])
check("replay: stale unlock never reset", counter(s) == 15, f"count={counter(s)}")

# --- fresh review (written AFTER the last probe) resets the loop ---
time.sleep(0.05)
with open(REVIEW_FILE, "w") as f:
    f.write("fresh evidence table: seam reviewed, hypotheses A/B/C " * 5)
out = run(bash(probe_cmd, s))
check("fresh review resets", kind(out) == "silent" and counter(s) == 0,
      f"kind={kind(out)} count={counter(s)}")

os.remove(REVIEW_FILE) if os.path.exists(REVIEW_FILE) else None
if saved_review is not None:
    open(REVIEW_FILE, "wb").write(saved_review)

print()
if failures:
    print(f"{len(failures)} FAILURES: {failures}")
    sys.exit(1)
print("all probe-loop-guard tests pass")
