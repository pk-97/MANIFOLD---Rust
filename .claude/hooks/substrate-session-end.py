#!/usr/bin/env python3
"""SessionEnd hook: tell the observer daemon its session is truly over.

Deliberately NOT wired to the Stop event — Stop fires once per agent turn
in this Claude Code version, not at session end, so using it to kill the
daemon would end observation after the first turn of any multi-turn
session. SessionEnd fires exactly once, at real termination (clear, logout,
prompt_input_exit, etc.), which is the correct signal for DESIGN.md §1's
"bookend: signals the daemon (final verdict, cleanup)".

Writes a stop-sentinel the daemon's poll loop checks every cycle, and
best-effort SIGTERMs the pidfile'd process so cleanup doesn't wait for the
10-minute idle timeout. SessionEnd has no decision control and this never
raises — pure best-effort cleanup.
"""
import json
import os
import signal
import subprocess
import sys

HOOKS_DIR = os.path.dirname(os.path.abspath(__file__))
SUBSTRATE_DIR = os.path.normpath(os.path.join(HOOKS_DIR, "..", "substrate"))
VERDICTS_DIR = os.path.join(SUBSTRATE_DIR, "verdicts")


def main():
    try:
        data = json.load(sys.stdin)
    except Exception:
        return
    session_id = data.get("session_id")
    if not session_id:
        return

    try:
        os.makedirs(VERDICTS_DIR, exist_ok=True)
        open(os.path.join(VERDICTS_DIR, f"{session_id}.stop"), "w").close()
    except OSError:
        pass

    try:
        with open(os.path.join(VERDICTS_DIR, f"{session_id}.pid")) as f:
            pid = int(f.read().strip())
        # A stale pidfile (crashed daemon) can point at a recycled PID owned
        # by anything — verify it's actually the observer before signaling.
        # If in doubt, skip the kill: the .stop sentinel above stops a live
        # daemon within one 3s poll anyway; SIGTERM is only fast cleanup.
        cmdline = subprocess.run(
            ["ps", "-p", str(pid), "-o", "command="],
            capture_output=True, text=True, timeout=5,
        ).stdout
        if "observer.py" in cmdline:
            os.kill(pid, signal.SIGTERM)
    except Exception:
        pass


if __name__ == "__main__":
    main()
    sys.exit(0)
