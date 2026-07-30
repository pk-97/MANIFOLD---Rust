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
             tool_input: dict | None = None, env_off: bool = False,
             human: bool = False, filler_bytes: int = 0) -> str:
    payload = {"tool_name": tool_name, "tool_input": tool_input or {}}
    if size is not None:
        tf = tempfile.NamedTemporaryFile(
            "w", suffix=".jsonl", delete=False, encoding="utf-8"
        )
        if human:
            tf.write(json.dumps({"type": "user", "origin": {"kind": "human"},
                                 "promptSource": "sdk"}) + "\n")
        if filler_bytes:
            pad = json.dumps({"type": "system", "pad": "x" * 900}) + "\n"
            tf.write(pad * (filler_bytes // len(pad) + 1))
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

# --- Human-seat exemption (Peter 2026-07-25) --------------------------------
check("opus conversation seat at 250K: silent",
      run_main(250_000, "claude-opus-5", human=True).strip() == "")
check("worker-model seat Peter types into at 250K: silent",
      run_main(250_000, "glm-4.7", human=True).strip() == "")
check("human turn far from the tail still exempts (whole-file scan)",
      run_main(250_000, "glm-4.7", human=True, filler_bytes=3_000_000).strip() == "")
check("no human turn at 250K: still denied",
      '"deny"' in run_main(250_000, "glm-4.7", human=False))

# --- Against real transcripts: conversation seats vs lane seats -------------
PROJ = Path.home() / ".claude/projects/-Users-peterkiemann-MANIFOLD---Rust"
if PROJ.is_dir():
    real = sorted(PROJ.glob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True)[:40]
    seats = {p: hook.is_conversation_seat(str(p)) for p in real}
    # A lane transcript's user turns are promptSource sdk/none with no human origin;
    # classify independently here so the assertion is not the hook grading itself.
    def typed_by_human(p: Path) -> bool:
        def anywhere(node) -> bool:
            # Nested too: a queued command carries origin inside an attachment row.
            if isinstance(node, dict):
                o = node.get("origin")
                if isinstance(o, dict) and o.get("kind") == "human":
                    return True
                return any(anywhere(v) for v in node.values())
            if isinstance(node, list):
                return any(anywhere(v) for v in node)
            return False

        for line in p.open(errors="replace"):
            try:
                e = json.loads(line)
            except ValueError:
                continue
            if anywhere(e):
                return True
        return False
    mismatches = [p.name for p in real if seats[p] != typed_by_human(p)]
    check(f"real transcripts classified correctly ({len(real)} files)", not mismatches)
    n_conv = sum(seats.values())
    check("real corpus has both classes", 0 < n_conv < len(real))
else:
    print("SKIP real-transcript check (project dir not found)")

# --- Fail-open plumbing ------------------------------------------------------
check("missing transcript: silent", run_main(None, "glm-4.7").strip() == "")
check("env off: silent", run_main(250_000, "glm-4.7", env_off=True).strip() == "")

print()
if FAILURES:
    print(f"{len(FAILURES)} FAILURES: {FAILURES}")
    sys.exit(1)
print("all tests passed")
