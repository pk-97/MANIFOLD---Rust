#!/usr/bin/env python3
"""Standalone test runner for agent-tier-spawn-guard.py (D-48 native lanes).

Invokes decide()/main() directly with synthetic input — never spawns a real
hook subprocess against a live session.

Run: python3 .claude/hooks/test_agent_tier_spawn_guard.py
"""
import importlib.util
import io
import json
import sys
import tempfile
from contextlib import redirect_stdout
from pathlib import Path

HOOKS_DIR = Path(__file__).resolve().parent
HOOK_PATH = HOOKS_DIR / "agent-tier-spawn-guard.py"
spec = importlib.util.spec_from_file_location("agent_tier_spawn_guard", HOOK_PATH)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

FAILURES = []


def check(name: str, cond: bool) -> None:
    print(("PASS " if cond else "FAIL ") + name)
    if not cond:
        FAILURES.append(name)


# --- decide(): tier rules -------------------------------------------------

# Executor tier: all spawns denied regardless of target slot.
for model in ("deepseek-v4-flash", "kimi-for-coding", "kimi-k2.7-code",
              "claude-sonnet-5", "claude-haiku-4-5-20251001"):
    for slot in ("haiku", "sonnet", ""):
        r = hook.decide(model, slot)
        check(f"executor denied: {model} -> {slot or '(none)'}",
              bool(r) and "executor" in r)

# Dispatcher tier (both GLM versions): haiku only.
for model in ("glm-4.7", "glm-5.2"):
    check(f"dispatcher allowed: {model} -> haiku",
          hook.decide(model, "haiku") == "")
    for slot in ("sonnet", "opus", "fable", ""):
        r = hook.decide(model, slot)
        check(f"dispatcher denied: {model} -> {slot or '(none)'}",
              bool(r) and "dispatcher" in r)

# Lead tier: anything goes.
for model in ("claude-fable-5", "claude-opus-4-8", "k3"):
    for slot in ("haiku", "sonnet", "opus", "fable", ""):
        check(f"lead allowed: {model} -> {slot or '(none)'}",
              hook.decide(model, slot) == "")

# Unidentifiable caller: fail open.
check("no model -> allow", hook.decide("", "sonnet") == "")

# --- main(): end-to-end with synthetic transcript --------------------------

def run_main(model_line: str | None, spawn_slot: str) -> str:
    payload = {"tool_input": {"model": spawn_slot, "prompt": "x",
                              "description": "t"}}
    if model_line is not None:
        tf = tempfile.NamedTemporaryFile(
            "w", suffix=".jsonl", delete=False, encoding="utf-8"
        )
        tf.write(json.dumps({"message": {"model": model_line}}) + "\n")
        tf.close()
        payload["transcript_path"] = tf.name
    sys.stdin = io.StringIO(json.dumps(payload))
    out = io.StringIO()
    try:
        with redirect_stdout(out):
            hook.main()
    except SystemExit:
        pass
    return out.getvalue()


out = run_main("deepseek-v4-flash", "haiku")
check("main: executor deny emitted", '"deny"' in out)
out = run_main("glm-5.2", "haiku")
check("main: dispatcher haiku passes silently", out.strip() == "")
out = run_main("glm-5.2", "opus")
check("main: dispatcher opus deny emitted", '"deny"' in out)
out = run_main("claude-fable-5", "sonnet")
check("main: lead passes silently", out.strip() == "")
out = run_main(None, "sonnet")
check("main: missing transcript fails open", out.strip() == "")

print()
if FAILURES:
    print(f"{len(FAILURES)} FAILURES: {FAILURES}")
    sys.exit(1)
print("all tests passed")