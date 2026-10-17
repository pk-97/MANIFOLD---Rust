#!/usr/bin/env python3
"""Tests for wrap-up-stop-guard.py — run: python3 test_wrap_up_stop_guard.py"""
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

HOOK = Path(__file__).with_name("wrap-up-stop-guard.py")
LEAD = {"type": "attachment", "attachment": {"content": "tools: mcp__ccd_turn__wrap_up"}}


def user(text="go"):
    return {"type": "user", "message": {"role": "user", "content": text}}


def tool_use(name):
    return {"type": "assistant", "message": {"content": [{"type": "tool_use", "name": name, "input": {}}]}}


def tool_result():
    return {"type": "user", "message": {"content": [{"type": "tool_result", "content": "ok"}]}}


def text(t):
    return {"type": "assistant", "message": {"content": [{"type": "text", "text": t}]}}


MARKERS = tempfile.mkdtemp()


def env(lead_env=False):
    e = {k: v for k, v in os.environ.items() if k != "CLAUDE_CODE_TERMINAL_MCP_TOOLS"}
    e["WRAP_UP_MARKER_DIR"] = MARKERS
    if lead_env:
        e["CLAUDE_CODE_TERMINAL_MCP_TOOLS"] = "mcp__ccd_turn__wrap_up"
    return e


def hook(payload, lead_env=False):
    p = subprocess.run([sys.executable, str(HOOK)], input=json.dumps(payload),
                       capture_output=True, text=True, env=env(lead_env))
    assert p.returncode == 0, p.stderr
    return bool(p.stdout.strip()) and json.loads(p.stdout)["decision"] == "block"


def transcript(entries):
    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
        f.write("\n".join(json.dumps(e) for e in entries))
    return f.name


def run(entries, active=False):
    return hook({"transcript_path": transcript(entries), "stop_hook_active": active})


def stop(path, sid, pid, lead_env=False):
    return hook({"hook_event_name": "Stop", "transcript_path": path, "session_id": sid, "prompt_id": pid},
                lead_env)


def wrapped(sid, pid):
    hook({"hook_event_name": "PostToolUse", "tool_name": "mcp__ccd_turn__wrap_up", "session_id": sid,
          "prompt_id": pid})


LONG = "\n".join(f"line {i}" for i in range(10))
cases = [
    ("tools without wrap_up blocks", [LEAD, user(), tool_use("Bash"), tool_result(), text("done")], False, True),
    ("tools with wrap_up allowed", [LEAD, user(), tool_use("Bash"), tool_result(), tool_use("mcp__ccd_turn__wrap_up"), text("x")], False, False),
    ("only ToolSearch and short reply allowed", [LEAD, user(), tool_use("ToolSearch"), tool_result(), text("hi")], False, False),
    ("long reply without tools blocks", [LEAD, user(), text(LONG)], False, True),
    ("short chat allowed", [LEAD, user(), text("sure")], False, False),
    ("stop_hook_active allowed", [LEAD, user(), tool_use("Bash")], True, False),
    ("earlier turn's wrap_up does not count", [LEAD, user(), tool_use("mcp__ccd_turn__wrap_up"), user("again"), tool_use("Edit")], False, True),
    ("no wrap_up tool in session (not desktop lead) skips", [user(), tool_use("Bash")], False, False),
]
for name, entries, active, want in cases:
    got = run(entries, active)
    assert got == want, f"{name}: got {got}"
    print("ok", name)

unflushed = transcript([LEAD, user(), tool_use("Bash"), tool_result()])
wrapped("s1", "p1")
assert not stop(unflushed, "s1", "p1"), "marker should prove wrap_up before the transcript flushes"
print("ok marker beats unflushed transcript")

assert stop(unflushed, "s1", "p2"), "a marker from an earlier prompt must not count"
print("ok earlier prompt's marker does not count")

assert stop(unflushed, "s2", "p1") and stop(unflushed, "s2", "p1") and not stop(unflushed, "s2", "p1")
print("ok blocks twice per prompt, then lets go")

no_name = transcript([user(), tool_use("Bash"), tool_result()])
assert stop(no_name, "s3", "p1", lead_env=True), "desktop env var marks a lead session"
assert not stop(no_name, "s4", "p1"), "no env var and no name in transcript skips"
print("ok desktop env var detects lead session")

with tempfile.NamedTemporaryFile("w", suffix=".jsonl") as f:
    f.write("not json\n{")
    f.flush()
    p = subprocess.run([sys.executable, str(HOOK)], input=json.dumps({"transcript_path": f.name}),
                       capture_output=True, text=True)
    assert p.returncode == 0 and not p.stdout.strip()
print("ok malformed fails open")
