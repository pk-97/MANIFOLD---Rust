#!/usr/bin/env python3
"""Tests for verbosity-gate.py: which row counts as the prompt, and the cap.

Run: python3 .claude/hooks/test_verbosity_gate.py
"""
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("vgate", HERE / "verbosity-gate.py")
vgate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(vgate)

FAILS = []


def check(name, cond):
    print(("ok   " if cond else "FAIL ") + name)
    if not cond:
        FAILS.append(name)


def user(text, uuid="u1", meta=False):
    return {"type": "user", "uuid": uuid, "isMeta": meta,
            "message": {"content": text}}


def tool_result(uuid="t1"):
    return {"type": "user", "uuid": uuid, "toolUseResult": {"stdout": "x"},
            "message": {"content": [{"type": "tool_result", "content": "x"}]}}


def assistant(text, uuid="a1"):
    return {"type": "assistant", "uuid": uuid,
            "message": {"content": [{"type": "text", "text": text}]}}


_STATE_DIR = tempfile.mkdtemp(prefix="vgate-state-")


def run(rows, session_id, last_assistant_message=None):
    """Invoke the hook as a subprocess; return (exit_code, stderr)."""
    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as fh:
        for r in rows:
            fh.write(json.dumps(r) + "\n")
        path = fh.name
    payload = {"transcript_path": path, "session_id": session_id}
    if last_assistant_message is not None:
        payload["last_assistant_message"] = last_assistant_message
    proc = subprocess.run(
        [sys.executable, str(HERE / "verbosity-gate.py")],
        input=json.dumps(payload),
        capture_output=True, text=True,
        env={**os.environ, "VERBOSITY_GATE_STATE": str(Path(_STATE_DIR) / "state.json")},
    )
    return proc.returncode, proc.stderr


LONG = "\n".join(f"line {i} of padded filler prose that says very little" for i in range(20))
SHORT = "Done. Two lines changed."

# --- prompt resolution -------------------------------------------------------
rows = [user("explain the design in detail", "real"), tool_result(), assistant(LONG)]
check("skips tool_result rows", vgate._last_real_user(rows)["uuid"] == "real")

rows = [user("less verbose", "real"),
        user("Stop hook feedback: Over budget", "meta", meta=True),
        assistant(LONG)]
check("skips the gate's own feedback", vgate._last_real_user(rows)["uuid"] == "real")

check("no prompt at all -> None", vgate._last_real_user([assistant(LONG)]) is None)

# --- detail cue reaches the budget through a tool-using turn ----------------
MID = "\n".join(f"line {i} of prose" for i in range(16))  # over normal, under detail
detail = [user("explain why this shimmers, in detail", "d1"), tool_result(), assistant(MID)]
code, err = run(detail, "sess-detail")
check("detail-cued 16-line answer passes", code == 0 and not err.strip())

normal = [user("did it land?", "n1"), tool_result(), assistant(MID)]
code, err = run(normal, "sess-normal-1")
check("uncued 16-line answer blocks", code == 2 and "Over budget" in err)

code, _ = run([user("did it land?", "n2"), tool_result(), assistant(SHORT)], "sess-short")
check("short answer passes", code == 0)

# --- the cap is per turn, and the gate's own feedback does not reset it ------
turn = [user("summarise", "cap1"), tool_result(), assistant(LONG)]
codes = []
for i in range(4):
    codes.append(run(turn, "sess-cap")[0])
    turn = turn + [user("Stop hook feedback: Over budget", f"m{i}", meta=True), assistant(LONG)]
check(f"blocks exactly twice then passes (got {codes})", codes == [2, 2, 0, 0])

# --- the harness-provided last_assistant_message wins, races the transcript -
# Regression for the bug where the hook measured the PREVIOUS turn: it derived
# "the message that just finished" by scanning the transcript file, which the
# Stop hook can read before that turn's write has flushed. Simulated here by a
# transcript that only contains the earlier (long, over-budget) turn, plus a
# payload last_assistant_message carrying the real, short, just-finished reply
# that hasn't hit disk yet.
race_rows = [user("did it land?", "race1"), tool_result(), assistant(LONG, "prev")]
code, err = run(race_rows, "sess-race-old", last_assistant_message=None)
check("old-code path (no payload field): stale transcript row blocks",
      code == 2 and "Over budget" in err)

code, err = run(race_rows, "sess-race-new", last_assistant_message=SHORT)
check("payload's last_assistant_message overrides the stale transcript row",
      code == 0 and not err.strip())

# A shorter rewrite must never score higher than the reply it replaced — that
# was the observed symptom (166 -> rewritten shorter -> 202). Same transcript,
# same stale previous-turn row, only the payload's actual final text differs.
_, err_long_stale = run(race_rows, "sess-shorter-1", last_assistant_message=None)
_, err_short_new = run(race_rows, "sess-shorter-2", last_assistant_message=SHORT)
check("shorter rewrite never scores higher via the stale path",
      "Over budget" in err_long_stale and not err_short_new.strip())

# --- fences: closed, unclosed, and indented ----------------------------------
FENCED = "one line of prose\n```\nthis is code and should not count at all\nneither should this\n```\nlast line of prose"
check("closed fence excludes its contents", vgate._measure(FENCED) == (2, 8))

UNCLOSED = "prose line one\n```\ncode that never gets closed\nmore code\nstill code"
check("unclosed fence hides everything after it (undercounts, not over)",
      vgate._measure(UNCLOSED) == (1, 3))

INDENTED_FENCE = "prose line\n    ```\n    fenced code, indented\n    ```\nmore prose"
check("indented fence delimiters are still recognised",
      vgate._measure(INDENTED_FENCE) == (2, 4))

# --- fails open --------------------------------------------------------------
proc = subprocess.run([sys.executable, str(HERE / "verbosity-gate.py")],
                      input="not json", capture_output=True, text=True)
check("garbage stdin fails open", proc.returncode == 0)

print()
print(f"{len(FAILS)} failure(s)" if FAILS else "all pass")
sys.exit(1 if FAILS else 0)
