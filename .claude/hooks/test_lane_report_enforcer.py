#!/usr/bin/env python3
"""Standalone test runner for lane-report-enforcer.py.

Builds synthetic parent/lane transcripts in a temp dir and calls decide()
directly — never touches a live session or the real state file.

Run: python3 .claude/hooks/test_lane_report_enforcer.py
"""
import importlib.util
import json
import tempfile
from pathlib import Path

HOOK_PATH = Path(__file__).resolve().parent / "lane-report-enforcer.py"
spec = importlib.util.spec_from_file_location("lane_report_enforcer", HOOK_PATH)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

FAILURES = []


def check(name: str, cond: bool) -> None:
    print(("PASS " if cond else "FAIL ") + name)
    if not cond:
        FAILURES.append(name)


def write_transcript(dirpath: Path, name: str, events: list) -> str:
    """events: list of ("text", body) / ("send", body) tuples -> assistant turns."""
    lines = []
    for kind, body in events:
        if kind == "text":
            content = [{"type": "text", "text": body}]
        else:
            content = [
                {"type": "tool_use", "name": "SendMessage", "input": {"to": "team-lead", "message": body}}
            ]
        lines.append(json.dumps({"message": {"role": "assistant", "content": content}}))
    p = dirpath / name
    p.write_text("\n".join(lines) + "\n")
    return str(p)


TMP = Path(tempfile.mkdtemp(prefix="lane-enforcer-test-"))
LEAD_PARENT = write_transcript(TMP, "lead_parent.jsonl", [("text", "SEAT IDENTITY: this session runs model k3 in the LEAD seat of the roster")])
DISPATCH_PARENT = write_transcript(TMP, "dispatch_parent.jsonl", [("text", "some other session context")])

REPORT = "Done — committed abc1234 on lane/x. Both gates pass. Verdict at /tmp/v.md."


# --- LEAD parent: final text turn is the report -----------------------------

lane = write_transcript(TMP, "lane1.jsonl", [("text", "working..."), ("text", REPORT)])
fb, loud = hook.decide("t1", LEAD_PARENT, lane, {})
check("lead parent + final text report -> allow", fb is None and loud is None)

lane = write_transcript(TMP, "lane2.jsonl", [("text", REPORT), ("send", REPORT)])
fb, loud = hook.decide("t2", LEAD_PARENT, lane, {})
check("lead parent + SendMessage AND final text -> allow but LOUD double-report", fb is None and loud is not None and "twice" in loud)

lane = write_transcript(TMP, "lane3.jsonl", [("text", "working on it")])
state = {}
fb, loud = hook.decide("t3", LEAD_PARENT, lane, state)
check("lead parent + no report -> blocked with final-text instructions", fb is not None and "auto-delivers" in fb and "Do NOT SendMessage" in fb)
check("block counted", state.get("t3") == 1)

# Oversized report: one compression bounce, then allow.
lane = write_transcript(TMP, "lane4.jsonl", [("text", "x" * 4000)])
state = {}
fb, loud = hook.decide("t4", LEAD_PARENT, lane, state)
check("lead parent + oversized report -> one bounce", fb is not None and "compressed" in fb)
fb, loud = hook.decide("t4", LEAD_PARENT, lane, state)
check("second oversized attempt -> allowed (never loop)", fb is None and loud is None)

# MAX_BLOCKS: stop bouncing, loud allow.
lane = write_transcript(TMP, "lane5.jsonl", [])
state = {"t5": hook.MAX_BLOCKS}
fb, loud = hook.decide("t5", LEAD_PARENT, lane, state)
check("MAX_BLOCKS reached -> loud allow", fb is None and loud is not None and "LOST" in loud)

# "nothing to report" passes the floor.
lane = write_transcript(TMP, "lane6.jsonl", [("text", "Nothing to report — told to stop.")])
fb, loud = hook.decide("t6", LEAD_PARENT, lane, {})
check("short 'nothing to report' allowed", fb is None and loud is None)


# --- UNKNOWN parent: old SendMessage mandate stays --------------------------

lane = write_transcript(TMP, "lane7.jsonl", [("text", REPORT)])
fb, loud = hook.decide("t7", DISPATCH_PARENT, lane, {})
check("unknown parent + no SendMessage -> blocked with SendMessage mandate", fb is not None and "SendMessage" in fb)

lane = write_transcript(TMP, "lane8.jsonl", [("send", REPORT)])
fb, loud = hook.decide("t8", DISPATCH_PARENT, lane, {})
check("unknown parent + SendMessage -> allow", fb is None and loud is None)

lane = write_transcript(TMP, "lane9.jsonl", [("send", "x" * 4000)])
state = {}
fb, loud = hook.decide("t9", DISPATCH_PARENT, lane, state)
check("unknown parent + oversized SendMessage -> one bounce", fb is not None and "compressed" in fb)
fb, loud = hook.decide("t9", DISPATCH_PARENT, lane, state)
check("unknown parent second oversized -> allowed", fb is None and loud is None)

# parent_is_lead detection.
check("lead marker detected", hook.parent_is_lead(LEAD_PARENT) is True)
check("no marker -> not lead", hook.parent_is_lead(DISPATCH_PARENT) is False)
check("missing parent file -> not lead (fail safe)", hook.parent_is_lead("/nonexistent.jsonl") is False)

print()
if FAILURES:
    print(f"{len(FAILURES)} FAILURE(S)")
    raise SystemExit(1)
print("ALL PASS")
