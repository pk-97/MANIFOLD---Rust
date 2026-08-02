#!/usr/bin/env python3
"""Standalone test runner for scripts/fleet_spawn.py (spawn-liveness exit codes).

Stubs `cc-fleet` on PATH with a shell script emitting canned envelopes, so the
wrapper's poll loop runs for real without a provider. Covers the 2026-08-02
failure: a dead provider key must exit 1 inside the grace window, not exit 0
with ok:true.

Run: python3 .claude/hooks/test_fleet_spawn.py
"""
import json
import os
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
WRAPPER = REPO / "scripts" / "fleet_spawn.py"

FAILURES = []


def check(name: str, cond: bool) -> None:
    print(("PASS " if cond else "FAIL ") + name)
    if not cond:
        FAILURES.append(name)


def make_stub(dirpath: Path, status_payload: dict, spawn_payload: dict) -> None:
    """cc-fleet stub: `subagent ... --json` prints spawn_payload (and records
    the call count); `subagent-status <id> --json` prints status_payload."""
    counter = dirpath / "calls"
    stub = dirpath / "cc-fleet"
    stub.write_text(
        "#!/bin/bash\n"
        f"echo x >> {counter}\n"
        'if [ "$1" = "subagent-status" ]; then\n'
        f"  echo '{json.dumps(status_payload)}'\n"
        "else\n"
        f"  echo '{json.dumps(spawn_payload)}'\n"
        "fi\n"
    )
    stub.chmod(stub.stat().st_mode | stat.S_IEXEC)


def run_wrapper(dirpath: Path, grace: int = 15) -> subprocess.CompletedProcess:
    env = dict(os.environ)
    env["PATH"] = f"{dirpath}:{os.environ['PATH']}"
    return subprocess.run(
        [sys.executable, str(WRAPPER), "opencode", "--model", "strong",
         "--prompt", "x", "--grace", str(grace)],
        capture_output=True, text=True, env=env, timeout=120,
    )


SPAWN_OK = {"ok": True, "job_id": "job-1", "status": "running"}
DEAD = {"ok": False, "status": "failed", "error_code": "KEY_INVALID",
        "error_msg": "provider rejected the API key (HTTP 401/403)",
        "suggestion": "Rotate the provider API key"}
ALIVE = {"ok": True, "status": "running"}

with tempfile.TemporaryDirectory() as td:
    d = Path(td)

    make_stub(d, DEAD, SPAWN_OK)
    p = run_wrapper(d)
    check("dead-at-spawn job exits 1", p.returncode == 1)
    check("dead-at-spawn names the error code", "KEY_INVALID" in p.stderr)

with tempfile.TemporaryDirectory() as td:
    d = Path(td)

    make_stub(d, ALIVE, SPAWN_OK)
    p = run_wrapper(d, grace=12)
    check("live job exits 0 after grace", p.returncode == 0)
    try:
        env = json.loads(p.stdout)
        check("live job hands back the job_id", env.get("job_id") == "job-1")
    except ValueError:
        check("live job stdout is the envelope", False)

with tempfile.TemporaryDirectory() as td:
    d = Path(td)

    make_stub(d, ALIVE, {"ok": False, "error_msg": "boom"})
    p = run_wrapper(d)
    check("failed spawn call exits 1 immediately", p.returncode == 1)
    # poll never happens: only the spawn call recorded
    calls = (d / "calls").read_text().count("x")
    check("failed spawn call skips polling", calls == 1)

print()
if FAILURES:
    print(f"{len(FAILURES)} FAILURES: {FAILURES}")
    sys.exit(1)
print("all tests passed")
