#!/usr/bin/env python3
"""Tests for hook_telemetry.py — run: python3 test_hook_telemetry.py"""
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

RUNNER = Path(__file__).with_name("hook_telemetry.py")

HOOKS = {
    "allow.py": 'import json,sys; json.load(sys.stdin); print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow", "permissionDecisionReason": "fine"}}))',
    "deny.py": 'import json; print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "no"}}))',
    "ctx.py": 'import json; print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "additionalContext": "note"}}))',
    "plain.py": 'print("plain context")',
    "block2.py": 'import sys; print("stop right there", file=sys.stderr); sys.exit(2)',
    "crash.py": 'raise RuntimeError("kaboom")',
    "silent.py": 'import sys; sys.stdin.read()',
    "subproc.py": 'import subprocess; subprocess.run(["echo", "leaked"])',
    "chdir.py": 'import os; os.chdir("/")',
    "reads_stdin.py": 'import json,sys; d=json.load(sys.stdin); print(json.dumps({"hookSpecificOutput": {"hookEventName": d["hook_event_name"], "additionalContext": d["tool_name"]}}))',
}


def run(tmp: Path, event: str, *hooks):
    payload = {"hook_event_name": event, "tool_name": "Bash", "tool_input": {"command": "ls"}}
    p = subprocess.run([sys.executable, str(tmp / "hook_telemetry.py"), *hooks],
                       input=json.dumps(payload), capture_output=True, text=True, cwd=tmp)
    out = json.loads(p.stdout) if p.stdout.strip().startswith("{") else p.stdout
    return p.returncode, out, p.stderr


def main() -> int:
    tmp = Path(tempfile.mkdtemp())
    hooks_dir = tmp / "hooks"
    hooks_dir.mkdir()
    shutil.copy(RUNNER, hooks_dir / "hook_telemetry.py")
    for name, src in HOOKS.items():
        (hooks_dir / name).write_text(src + "\n")
    t = hooks_dir
    fails = []

    def check(label, cond):
        if not cond:
            fails.append(label)

    code, out, _ = run(t, "PreToolUse", "allow.py")
    check("single hook mirrored verbatim", code == 0 and out["hookSpecificOutput"]["permissionDecision"] == "allow")

    code, out, _ = run(t, "PreToolUse", "allow.py", "deny.py", "ctx.py")
    h = out["hookSpecificOutput"]
    check("deny beats allow", h["permissionDecision"] == "deny")
    check("reason from winning level only", h["permissionDecisionReason"] == "no")
    check("context kept alongside verdict", h["additionalContext"] == "note")
    check("event name set", h["hookEventName"] == "PreToolUse")

    code, out, err = run(t, "PreToolUse", "allow.py", "block2.py")
    check("exit 2 blocks the group", code == 2 and "stop right there" in err)

    code, out, _ = run(t, "PreToolUse", "crash.py", "deny.py")
    check("crash fails open, sibling deny stands",
          code == 0 and out["hookSpecificOutput"]["permissionDecision"] == "deny"
          and "crash.py failed open" in out["systemMessage"])

    code, out, err = run(t, "PreToolUse", "crash.py")
    check("single crash keeps old exit 1", code == 1 and "kaboom" in err)

    code, out, _ = run(t, "SessionStart", "plain.py", "ctx.py")
    check("plain text becomes context on SessionStart",
          out["hookSpecificOutput"]["additionalContext"] == "plain context\n\nnote")

    code, out, _ = run(t, "SessionStart", "plain.py", "silent.py")
    check("plain text alone still reaches context",
          code == 0 and out["hookSpecificOutput"]["additionalContext"] == "plain context")

    code, out, err = run(t, "PreToolUse", "subproc.py", "ctx.py")
    check("subprocess output cannot corrupt stdout",
          isinstance(out, dict) and "leaked" not in json.dumps(out) and "leaked" in err)

    code, out, _ = run(t, "PreToolUse", "chdir.py", "reads_stdin.py", "reads_stdin.py")
    check("each hook gets full stdin", out["hookSpecificOutput"]["additionalContext"] == "Bash\n\nBash")

    code, out, _ = run(t, "PreToolUse", "silent.py", "silent.py")
    check("silent group emits nothing", code == 0 and out == "")

    log = [json.loads(l) for l in (tmp / "telemetry" / "hook-fires.jsonl").read_text().splitlines()]
    check("one telemetry line per hook", sum(1 for r in log if r["hook"] == "silent.py") == 3)

    shutil.rmtree(tmp)
    for f in fails:
        print(f"FAIL: {f}")
    print(f"{len(fails)} failed")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
