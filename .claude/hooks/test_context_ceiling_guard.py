#!/usr/bin/env python3
"""Standalone test runner for context-ceiling-guard.py (tier split, 2026-07-24).

Invokes main() directly with synthetic transcripts — never spawns a real
hook subprocess against a live session.

Run: python3 .claude/hooks/test_context_ceiling_guard.py
"""
import importlib.util
import io
import json
import os
import sys
import tempfile
from contextlib import redirect_stdout
from pathlib import Path

HOOKS_DIR = Path(__file__).resolve().parent
HOOK_PATH = HOOKS_DIR / "context-ceiling-guard.py"
spec = importlib.util.spec_from_file_location("context_ceiling_guard", HOOK_PATH)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

FAILURES = []


def check(name: str, cond: bool) -> None:
    print(("PASS " if cond else "FAIL ") + name)
    if not cond:
        FAILURES.append(name)


def run_main(size: int | None, model: str, tool_name: str = "Read",
             tool_input: dict | None = None, env_off: bool = False) -> str:
    payload = {"tool_name": tool_name, "tool_input": tool_input or {}}
    if size is not None:
        tf = tempfile.NamedTemporaryFile(
            "w", suffix=".jsonl", delete=False, encoding="utf-8"
        )
        entry = {"message": {"model": model,
                             "usage": {"cache_read_input_tokens": size,
                                       "cache_creation_input_tokens": 0,
                                       "input_tokens": 0}}}
        tf.write(json.dumps(entry) + "\n")
        tf.close()
        payload["transcript_path"] = tf.name
    if env_off:
        os.environ["MANIFOLD_CONTEXT_CEILING"] = "off"
    sys.stdin = io.StringIO(json.dumps(payload))
    out = io.StringIO()
    try:
        with redirect_stdout(out):
            hook.main()
    except SystemExit:
        pass
    finally:
        os.environ.pop("MANIFOLD_CONTEXT_CEILING", None)
    return out.getvalue()


# --- Lead tier: fully exempt, no warn, no deny ------------------------------
check("fable at 500K: silent", run_main(500_000, "claude-fable-5").strip() == "")
check("fable at 170K: no warn", run_main(170_000, "claude-fable-5").strip() == "")
check("k3 at 250K: silent", run_main(250_000, "k3").strip() == "")

# --- Opus is NOT lead (Peter 2026-07-24) ------------------------------------
check("opus at 250K: denied", '"deny"' in run_main(250_000, "claude-opus-4-8"))

# --- Worker tiers: unchanged 150K warn / 200K deny + wrap-up lane -----------
check("glm at 250K: denied", '"deny"' in run_main(250_000, "glm-4.7"))
check("deepseek at 250K: denied", '"deny"' in run_main(250_000, "deepseek-v4-flash"))
out = run_main(160_000, "deepseek-v4-flash")
check("deepseek at 160K: warn-allow", '"allow"' in out and "ceiling" in out.lower())
check("deepseek at 100K: silent", run_main(100_000, "deepseek-v4-flash").strip() == "")
check("sonnet at 210K: denied", '"deny"' in run_main(210_000, "claude-sonnet-5"))
out = run_main(250_000, "glm-4.7", tool_name="Bash",
               tool_input={"command": "git commit -m 'x' -- a.md"})
check("worker wrap-up lane: git allowed", '"allow"' in out)
out = run_main(250_000, "glm-4.7", tool_name="Write",
               tool_input={"file_path": "/x/.claude/orchestration/handoff.md"})
check("worker wrap-up lane: handoff write allowed", '"allow"' in out)

# --- Unidentifiable model: fail-strict on tier (worker rules) ---------------
check("no model at 250K: denied", '"deny"' in run_main(250_000, ""))

# --- Fail-open plumbing ------------------------------------------------------
check("missing transcript: silent", run_main(None, "glm-4.7").strip() == "")
check("env off: silent", run_main(250_000, "glm-4.7", env_off=True).strip() == "")

print()
if FAILURES:
    print(f"{len(FAILURES)} FAILURES: {FAILURES}")
    sys.exit(1)
print("all tests passed")
