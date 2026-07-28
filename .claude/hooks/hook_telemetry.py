#!/usr/bin/env python3
"""Telemetry pass-through runner for every registered hook.

Why: 30+ hooks, all fail-open, none observed. Whether a hook ever fires —
let alone ever denies/asks/injects — was an argument, not a lookup. Retiring
a rule (the "Obsolete when:" census) needs fire counts the same way retiring
a code path needs coverage. This runner is the counter.

settings.json invokes hooks as
    python3 .../hook_telemetry.py <hook-file.py>
instead of calling the hook directly. The runner pipes stdin through, mirrors
stdout/stderr exactly, exits with the hook's exit code — behaviorally
transparent — and appends one JSONL line per invocation to
.claude/telemetry/hook-fires.jsonl (gitignored via the .claude/* default):

    {"ts", "hook", "event", "exit", "out", "err", "ms"}

"Fired and acted" ≈ out > 0 or exit != 0; "fired silent" is out == 0, exit 0.
Dead-hook census: hooks registered vs hooks appearing in the log over a real
working window.

Fails OPEN twice over: a logging failure never blocks the hook's verdict, and
a runner failure to even launch the hook exits 0 (same fail-open contract
every hook here already has). No timeout imposed — the harness owns that.

Obsolete when: the harness itself reports per-hook invocation/decision
telemetry, or the hook census stops being a maintained practice.
"""
import json
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

_HOOKS_DIR = Path(__file__).resolve().parent
_LOG = _HOOKS_DIR.parent / "telemetry" / "hook-fires.jsonl"


def main():
    if len(sys.argv) != 2:
        print("hook_telemetry: usage: hook_telemetry.py <hook-file.py>",
              file=sys.stderr)
        return 0
    hook = _HOOKS_DIR / sys.argv[1]
    stdin_data = sys.stdin.buffer.read()

    start = time.time()
    try:
        r = subprocess.run(
            [sys.executable, str(hook)], input=stdin_data,
            capture_output=True,
        )
    except Exception as e:
        print(f"hook_telemetry failed open launching {hook.name}: {e}",
              file=sys.stderr)
        return 0
    ms = round((time.time() - start) * 1000)

    # Mirror the hook verbatim — the harness must see exactly what the hook
    # produced, no framing, no reordering.
    sys.stdout.buffer.write(r.stdout)
    sys.stderr.buffer.write(r.stderr)
    sys.stdout.buffer.flush()
    sys.stderr.buffer.flush()

    try:
        event = ""
        if stdin_data:
            try:
                event = json.loads(stdin_data).get("hook_event_name", "")
            except (json.JSONDecodeError, AttributeError):
                pass
        _LOG.parent.mkdir(parents=True, exist_ok=True)
        with open(_LOG, "a") as f:
            f.write(json.dumps({
                "ts": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                "hook": hook.name,
                "event": event,
                "exit": r.returncode,
                "out": len(r.stdout),
                "err": len(r.stderr),
                "ms": ms,
            }, sort_keys=True) + "\n")
    except Exception:
        pass  # telemetry must never change hook behavior

    return r.returncode


if __name__ == "__main__":
    sys.exit(main())
