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


# --- decide(): all spawn verbs denied for every tier (native lanes only) ---

# Every spawn verb denied regardless of caller tier (Peter 2026-08-02).
for verb in ("subagent", "run", "workflow"):
    for model in ("deepseek-v4-flash", "glm-4.7", "glm-5.2", "k3",
                  "claude-fable-5", "kimi-k2.7-code", "claude-sonnet-5", ""):
        r = hook.decide(f"cc-fleet {verb} opencode --prompt hi", model)
        check(f"all-tier denied: {verb} ({model or 'no model'})",
              bool(r) and "NATIVE" in r)
check(
    "spawn dead-path denied: executor",
    bool(hook.decide("cc-fleet spawn opencode --prompt hi", "deepseek-v4-flash")),
)

# D-48: `cc-fleet spawn` is a dead path — denied for every tier, lead
# included, even with no identifiable caller model.
for model in ("claude-fable-5", "k3", "glm-4.7", "deepseek-v4-flash", ""):
    r = hook.decide("cc-fleet spawn opencode --as w1 --team t --json", model)
    check(f"spawn dead-path denied: {model or '(no model)'}", bool(r) and "dead" in r)

# Non-spawn cc-fleet commands: never denied for anyone.
for cmd in ("cc-fleet list --json", "cc-fleet models opencode --json",
            "cc-fleet subagent-status abc", "cc-fleet keyget opencode"):
    check(f"non-spawn allowed: {cmd}", hook.decide(cmd, "deepseek-v4-flash") == "")

# Compound command still caught.
check(
    "denied inside compound",
    bool(hook.decide("git status && cc-fleet subagent opencode -p x", "deepseek-v4-flash")),
)

# Prose mentions (not command position) never match — the 2026-07-24 false
# positives: commit messages and rg patterns quoting the phrase.
for cmd in (
    "git commit -m 'guard: cc-fleet spawn denied for every tier' -- docs/x.md",
    "rg -l -i 'cc-fleet spawn|tmux teammate' docs/",
    "echo about cc-fleet spawn",
):
    check(f"prose mention allowed: {cmd[:40]}...",
          hook.decide(cmd, "claude-fable-5") == "" and
          hook.decide(cmd, "deepseek-v4-flash") == "")

# Command-position variants still caught.
for cmd in (
    "cc-fleet spawn opencode --as w --team t",
    "git status && cc-fleet spawn opencode --as w",
    "FOO=1 cc-fleet spawn opencode",
    "/Users/x/.local/bin/cc-fleet spawn opencode",
):
    check(f"command position caught: {cmd[:40]}...",
          bool(hook.decide(cmd, "claude-fable-5")))

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
check("main: lead deny emitted", '"deny"' in out)
out = run_main("cc-fleet subagent opencode -p x", None)
check("main: missing transcript fails open", out.strip() == "")
out = run_main("cargo build", "deepseek-v4-flash")
check("main: non-cc-fleet command untouched", out.strip() == "")
out = run_main("cc-fleet subagent-status 8f422e60 --json", "k3")
check("main: subagent-status poll passes silently", out.strip() == "")
out = run_main(
    "cc-fleet subagent opencode --model strong --background --json -p x", "k3"
)
check("main: lead background deny emitted", '"deny"' in out)

print()
if FAILURES:
    print(f"{len(FAILURES)} FAILURES: {FAILURES}")
    sys.exit(1)
print("all tests passed")
