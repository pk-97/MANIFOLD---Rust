#!/usr/bin/env python3
"""Telemetry pass-through runner for every registered hook.

Whether a hook ever fires — let alone ever denies/asks/injects — should be a lookup, not
an argument. Retiring a rule (the "Obsolete when:" census) needs fire counts the same
way retiring a code path needs coverage. This runner is the counter.

settings.json invokes hooks as
    python3 .../hook_telemetry.py <hook-file.py>
instead of calling the hook directly. The runner pipes stdin through, mirrors
stdout/stderr exactly, exits with the hook's exit code — behaviorally transparent — and
appends one JSONL line per invocation to .claude/telemetry/hook-fires.jsonl (gitignored
via the .claude/* default):

    {"ts", "hook", "event", "exit", "out", "err", "ms"}

"Fired and acted" is out > 0 or exit != 0; "fired silent" is out == 0, exit 0. Dead-hook
census: hooks registered vs hooks appearing in the log over a real working window.

Fails OPEN twice over: a logging failure never blocks the hook's verdict, and a runner
failure to even launch the hook exits 0. No timeout imposed — the harness owns that.

Obsolete when: the harness itself reports per-hook invocation/decision telemetry, or the
hook census stops being a maintained practice.
"""
import json
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

_HOOKS_DIR = Path(__file__).resolve().parent
_LOG = _HOOKS_DIR.parent / "telemetry" / "hook-fires.jsonl"


def _derive_decision(stdout: bytes):
    """Best-effort parse of hook stdout to derive a decision label.

    Returns a string or None. Never raises.
    """
    if not stdout:
        return None
    try:
        payload = json.loads(stdout)
    except (json.JSONDecodeError, UnicodeDecodeError):
        return None

    # Permission gates (PreToolUse / AskUserQuestion) carry their verdict
    # in hookSpecificOutput.permissionDecision.
    hso = payload.get("hookSpecificOutput")
    if isinstance(hso, dict):
        pd = hso.get("permissionDecision")
        if isinstance(pd, str) and pd:
            return pd

    # Stop / SubagentStop hooks carry a top-level decision.
    d = payload.get("decision")
    if isinstance(d, str) and d:
        return d

    # Context injection: the hook added additional context to the turn.
    if isinstance(hso, dict) and isinstance(hso.get("additionalContext"), str) and hso["additionalContext"]:
        return "context"
    if isinstance(payload.get("additionalContext"), str) and payload["additionalContext"]:
        return "context"

    return None


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

    # Derive decision from stdout before mirroring (the parse is best-effort).
    decision = _derive_decision(r.stdout)

    # Mirror the hook verbatim — the harness must see exactly what the hook
    # produced, no framing, no reordering.
    sys.stdout.buffer.write(r.stdout)
    sys.stderr.buffer.write(r.stderr)
    sys.stdout.buffer.flush()
    sys.stderr.buffer.flush()

    try:
        event = ""
        seat = {}
        if stdin_data:
            try:
                payload = json.loads(stdin_data)
                event = payload.get("hook_event_name", "")
                # Seat attribution (2026-07-28): the teammate-vs-lead payload
                # shape is the enforcement surface — guards mis-tiered a
                # teammate because payloads carry the PARENT transcript.
                # Record the discriminating fields so seat bugs are a lookup.
                for k in ("session_id", "teammate_name", "team_name", "tool_name"):
                    v = payload.get(k)
                    if v:
                        seat[k] = v
                # Full key inventory: the teammate-payload shape question
                # (does ANY field discriminate seats?) must be a lookup.
                seat["keys"] = ",".join(sorted(payload.keys()))
                # Command traceability (BUG-0x4w): a permission prompt must
                # be a lookup, not a reconstruction — record what was about
                # to run, truncated.
                if event == "PreToolUse":
                    ti = payload.get("tool_input") or {}
                    cmd = ti.get("command") or ti.get("file_path")
                    if isinstance(cmd, str) and cmd:
                        seat["cmd"] = cmd[:500]
            except (json.JSONDecodeError, AttributeError):
                pass
        record = {
            "ts": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "hook": hook.name,
            "event": event,
            "exit": r.returncode,
            "out": len(r.stdout),
            "err": len(r.stderr),
            "ms": ms,
            **seat,
        }
        if decision is not None:
            record["decision"] = decision
        _LOG.parent.mkdir(parents=True, exist_ok=True)
        with open(_LOG, "a") as f:
            f.write(json.dumps(record, sort_keys=True) + "\n")
    except Exception:
        pass  # telemetry must never change hook behavior

    return r.returncode


if __name__ == "__main__":
    sys.exit(main())
