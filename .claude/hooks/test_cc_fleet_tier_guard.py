#!/usr/bin/env python3
"""Standalone test runner for cc-fleet-tier-guard.py (R2, AGENT_ROUTING §0).

Invokes decide()/main() directly with synthetic input — never spawns a real
hook subprocess against a live session.

Run: python3 .claude/hooks/test_cc_fleet_tier_guard.py
"""
import importlib.util
import io
import json
import sys
import tempfile
from contextlib import redirect_stdout
from pathlib import Path

HOOKS_DIR = Path(__file__).resolve().parent
HOOK_PATH = HOOKS_DIR / "cc-fleet-tier-guard.py"
spec = importlib.util.spec_from_file_location("cc_fleet_tier_guard", HOOK_PATH)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

FAILURES = []


def check(name: str, cond: bool) -> None:
    print(("PASS " if cond else "FAIL ") + name)
    if not cond:
        FAILURES.append(name)


# --- decide(): tier rules -------------------------------------------------

# Executor tier: every spawn verb denied ("spawn" via the dead-path rule).
for verb in ("subagent", "run", "workflow"):
    r = hook.decide(f"cc-fleet {verb} opencode --prompt hi", "deepseek-v4-flash")
    check(f"executor denied: {verb}", bool(r) and "executor" in r)
check(
    "executor denied: spawn (dead path)",
    bool(hook.decide("cc-fleet spawn opencode --prompt hi", "deepseek-v4-flash")),
)
check(
    "executor denied: kimi-k2.7",
    bool(hook.decide("cc-fleet subagent opencode -p x", "kimi-k2.7-code")),
)
check(
    "executor denied: sonnet",
    bool(hook.decide("cc-fleet subagent opencode -p x", "claude-sonnet-5")),
)

# Dispatcher tier: may drive the executor provider via subagent, nothing else.
check(
    "dispatcher allowed: subagent opencode",
    hook.decide("cc-fleet subagent opencode --prompt-file b.md", "glm-4.7") == "",
)
check(
    "dispatcher allowed: subagent deepseek",
    hook.decide("cc-fleet subagent deepseek -p x", "glm-4.7") == "",
)
check(
    "dispatcher denied: subagent glm (self-tier)",
    bool(hook.decide("cc-fleet subagent glm -p x", "glm-4.7")),
)
check(
    "dispatcher denied: subagent kimi-code (lead seat)",
    bool(hook.decide("cc-fleet subagent kimi-code -p x", "glm-5.2")),
)
check(
    "dispatcher denied: workflow",
    bool(hook.decide("cc-fleet workflow --script s.js", "glm-4.7")),
)
check(
    "dispatcher denied: spawn",
    bool(hook.decide("cc-fleet spawn opencode", "glm-4.7")),
)

# Lead tier: passes through.
for model in ("claude-fable-5", "claude-opus-4-8", "k3"):
    check(
        f"lead allowed: {model}",
        hook.decide("cc-fleet subagent opencode -p x", model) == "",
    )

# D-48: `cc-fleet spawn` is a dead path — denied for every tier, lead
# included, even with no identifiable caller model.
for model in ("claude-fable-5", "k3", "glm-4.7", "deepseek-v4-flash", ""):
    r = hook.decide("cc-fleet spawn glm --as w1 --team t --json", model)
    check(f"spawn dead-path denied: {model or '(no model)'}", bool(r) and "dead" in r)

# Non-spawn cc-fleet commands: never denied for anyone.
for cmd in ("cc-fleet list --json", "cc-fleet models opencode --json",
            "cc-fleet subagent-status abc", "cc-fleet keyget opencode"):
    check(f"non-spawn allowed: {cmd}", hook.decide(cmd, "deepseek-v4-flash") == "")

# Compound command still caught.
check(
    "executor denied inside compound",
    bool(hook.decide("git status && cc-fleet subagent opencode -p x", "deepseek-v4-flash")),
)

# Empty model (unidentifiable caller): fail open.
check("no model -> allow", hook.decide("cc-fleet subagent opencode -p x", "") == "")

# --- main(): end-to-end with synthetic transcript --------------------------

def run_main(command: str, model_line: str | None) -> str:
    payload = {"tool_input": {"command": command}}
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


out = run_main("cc-fleet subagent opencode -p x", "deepseek-v4-flash")
check("main: executor deny emitted", '"deny"' in out)
out = run_main("cc-fleet subagent opencode -p x", "claude-fable-5")
check("main: lead passes silently", out.strip() == "")
out = run_main("cc-fleet subagent opencode -p x", None)
check("main: missing transcript fails open", out.strip() == "")
out = run_main("cargo build", "deepseek-v4-flash")
check("main: non-cc-fleet command untouched", out.strip() == "")

print()
if FAILURES:
    print(f"{len(FAILURES)} FAILURES: {FAILURES}")
    sys.exit(1)
print("all tests passed")
