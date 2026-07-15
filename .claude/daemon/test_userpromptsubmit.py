#!/usr/bin/env python3
"""Tests for the UserPromptSubmit hook (daemon-userpromptsubmit.py).

Covers the pending-mailbox-flag delivery it always had, plus the
grade-backstop and observation-review-prompt housekeeping nags that moved
here from the Stop hook 2026-07-15 (Peter's ruling — DESIGN.md §2k):
neither is a detection, so neither may block a turn from ending; they now
deliver as additionalContext on the next user prompt instead. Functions,
sentinels, and reason wording carried over unchanged from the old
test_stop_valve.py grade-backstop/observation-prompt tests — only the
delivery mechanism (additionalContext instead of "decision": "block") and
the entry point (UserPromptSubmit instead of Stop) differ.

Run: python3 test_userpromptsubmit.py
"""
import importlib.util
import json
import os
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

DAEMON_DIR = Path(__file__).resolve().parent
HOOK_PATH = DAEMON_DIR.parent / "hooks" / "daemon-userpromptsubmit.py"
sys.path.insert(0, str(DAEMON_DIR))
import valve  # noqa: E402

_direct_spec = importlib.util.spec_from_file_location("daemon_userpromptsubmit_direct", HOOK_PATH)
DIRECT = importlib.util.module_from_spec(_direct_spec)
_direct_spec.loader.exec_module(DIRECT)

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


def run_hook(stdin_obj, verdicts_dir):
    """Same in-process-fresh-exec pattern as test_stop_valve.py's run_hook —
    the hook module is reloaded fresh per call, but its own `import valve`
    hits the same cached module object this test file already imported, so
    monkeypatching `valve.*` here reaches the hook too."""
    spec = importlib.util.spec_from_file_location("daemon_userpromptsubmit", HOOK_PATH)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)

    import io

    orig_stdin, orig_stdout = sys.stdin, sys.stdout
    sys.stdin = io.StringIO(json.dumps(stdin_obj) if not isinstance(stdin_obj, str) else stdin_obj)
    sys.stdout = io.StringIO()
    try:
        mod.main()
        out = sys.stdout.getvalue()
    finally:
        sys.stdin = orig_stdin
        sys.stdout = orig_stdout
    return out.strip()


def with_temp_verdicts(fn):
    orig_verdicts = valve.VERDICTS_DIR
    orig_telemetry = valve.TELEMETRY_PATH
    orig_eval_dir = valve.EVAL_DIR
    orig_payload_cache = valve._PAYLOAD_CACHE
    orig_ensure_observer = valve.ensure_observer
    with tempfile.TemporaryDirectory() as td:
        valve.VERDICTS_DIR = td
        valve.TELEMETRY_PATH = os.path.join(td, "telemetry.jsonl")
        valve.EVAL_DIR = os.path.join(td, "eval")
        valve._PAYLOAD_CACHE = None
        valve.ensure_observer = lambda *a, **k: None  # never spawn a real subprocess in tests
        try:
            fn(td)
        finally:
            valve.VERDICTS_DIR = orig_verdicts
            valve.TELEMETRY_PATH = orig_telemetry
            valve.EVAL_DIR = orig_eval_dir
            valve._PAYLOAD_CACHE = orig_payload_cache
            valve.ensure_observer = orig_ensure_observer


def write_telemetry_records(records):
    os.makedirs(os.path.dirname(valve.TELEMETRY_PATH), exist_ok=True)
    with open(valve.TELEMETRY_PATH, "a", encoding="utf-8") as f:
        for r in records:
            f.write(json.dumps(r) + "\n")


def write_grade_records(records, filename="live_grades.session.jsonl"):
    os.makedirs(valve.EVAL_DIR, exist_ok=True)
    with open(os.path.join(valve.EVAL_DIR, filename), "a", encoding="utf-8") as f:
        for r in records:
            f.write(json.dumps(r) + "\n")


def write_transcript_with_events(path, n_events, start_ts, final_text="Here is the summary of what I found."):
    def iso(ts):
        return datetime.fromtimestamp(ts, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    lines = []
    for i in range(n_events):
        lines.append(json.dumps({
            "type": "user",
            "timestamp": iso(start_ts + i + 1),
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": f"t{i}", "content": "ok"}]},
        }))
    lines.append(json.dumps({
        "type": "assistant",
        "timestamp": iso(start_ts + n_events + 1),
        "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [{"type": "text", "text": final_text}]},
    }))
    with open(path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")


def write_verdict(verdicts_dir, key, move_id, seq, ts=None):
    path = os.path.join(verdicts_dir, f"{key}.json")
    with open(path, "w", encoding="utf-8") as f:
        json.dump({"ts": ts if ts is not None else time.time(), "flag": {"move_id": move_id, "seq": seq, "evidence": "test", "confidence": 0.9}}, f)


def plant_observation_prompt_fired(td, session):
    open(os.path.join(td, f"{session}.observation-prompt-fired"), "w").close()


def plant_grade_backstop_fired(td, session):
    open(os.path.join(td, f"{session}.grade-backstop-fired"), "w").close()


def context_of(out):
    d = json.loads(out) if out else None
    if not d:
        return None, d
    return d.get("hookSpecificOutput", {}).get("additionalContext", ""), d


# ---- pending mailbox flag (pre-existing behavior, unchanged) ----


def test_pending_flag_delivers_as_additional_context():
    def run(td):
        session = "sess-pending"
        write_verdict(td, session, "anchor/verify-claim", 1)
        out = run_hook({"session_id": session, "transcript_path": "/dev/null"}, td)
        ctx, d = context_of(out)
        check("pending flag delivers via additionalContext", ctx is not None and 'move="anchor/verify-claim"' in ctx, out)
        check("consumed marker written", valve.read_consumed(session) == 1)

    with_temp_verdicts(run)


def test_no_pending_flag_falls_through_to_housekeeping_checks():
    def run(td):
        session = "sess-nopending-short"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 5, 1783200000)
        out = run_hook({"session_id": session, "transcript_path": transcript}, td)
        check("nothing pending, session too short for any nag: silent", out == "", out)

    with_temp_verdicts(run)


# ---- grade-backstop (moved from test_stop_valve.py, wording/logic unchanged) ----


def test_session_gradeable_fires_filters_prefix_agent_and_null_seq():
    with tempfile.TemporaryDirectory() as td:
        tpath = os.path.join(td, "telemetry.jsonl")
        with open(tpath, "w", encoding="utf-8") as f:
            for rec in (
                {"event": "injected", "session_id": "s1", "agent_id": None, "move_id": "anchor/verify-claim", "seq": 2, "ts": 100},
                {"event": "injected", "session_id": "s1", "agent_id": None, "move_id": "mechanical/reasoning-primer", "seq": 1, "ts": 50},
                {"event": "injected", "session_id": "s1", "agent_id": "w1", "move_id": "anchor/thrash", "seq": 3, "ts": 150},
                {"event": "injected", "session_id": "s2", "agent_id": None, "move_id": "coaching/define-done", "seq": 1, "ts": 10},
                {"event": "injected", "session_id": "s1", "agent_id": None, "move_id": "escalate/checkpoint", "seq": 5, "ts": 5},
                {"event": "injected", "session_id": "s1", "agent_id": None, "move_id": "anchor/skim", "seq": None, "ts": 9},
                {"event": "observer_spawn", "session_id": "s1", "ts": 1},
            ):
                f.write(json.dumps(rec) + "\n")
        fires = DIRECT._session_gradeable_fires(tpath, "s1")
        check(
            "keeps only s1's own main-mailbox anchor/coaching/escalate fires with a seq, oldest first",
            [m for _, m, _ in fires] == ["anchor/verify-claim", "escalate/checkpoint"],
            fires,
        )


def test_session_grade_count_sums_across_live_grades_files():
    with tempfile.TemporaryDirectory() as td:
        eval_dir = os.path.join(td, "eval")
        os.makedirs(eval_dir)
        with open(os.path.join(eval_dir, "live_grades.jsonl"), "w", encoding="utf-8") as f:
            f.write(json.dumps({"session_id": "s1"}) + "\n")
        with open(os.path.join(eval_dir, "live_grades.session.jsonl"), "w", encoding="utf-8") as f:
            f.write(json.dumps({"session_id": "s1"}) + "\n")
            f.write(json.dumps({"session_id": "s2"}) + "\n")
        check("counts s1 records across both files", DIRECT._session_grade_count(eval_dir, "s1") == 2)
        check("missing eval dir returns 0, never raises", DIRECT._session_grade_count(os.path.join(td, "nope"), "s1") == 0)


def test_events_since_counts_tool_results_strictly_after_ts():
    with tempfile.TemporaryDirectory() as td:
        path = os.path.join(td, "t.jsonl")
        base = 1783200000
        write_transcript_with_events(path, 5, base)
        check("counts all events after an early since_ts", DIRECT._events_since(path, base) == 5)
        check("counts only events strictly after a later since_ts", DIRECT._events_since(path, base + 3) == 2)
        check("since_ts=None -> 0, never raises", DIRECT._events_since(path, None) == 0)


def test_grade_backstop_fires_as_additional_context_when_ungraded_and_stale():
    def run(td):
        session = "sess-backstop-fire"
        base = 1783200000
        write_telemetry_records([{"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/verify-claim"}])
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, base)
        out1 = run_hook({"session_id": session, "transcript_path": transcript}, td)
        ctx1, d1 = context_of(out1)
        check("backstop delivers as additionalContext when stale and ungraded", ctx1 is not None, out1)
        check("reason names the backstop move", ctx1 and 'move="mechanical/grade-backstop"' in ctx1, ctx1)
        check("reason states the ungraded count", ctx1 and "1 gradeable" in ctx1, ctx1)
        check("session-level sentinel written", os.path.exists(os.path.join(td, f"{session}.grade-backstop-fired")))

        plant_observation_prompt_fired(td, session)  # isolate: unrelated 40-event gate
        out2 = run_hook({"session_id": session, "transcript_path": transcript}, td)
        check("second turn stays silent (once per session, not per turn)", out2 == "", out2)

    with_temp_verdicts(run)


def test_grade_backstop_skips_when_not_stale_enough():
    def run(td):
        session = "sess-backstop-fresh"
        base = 1783200000
        write_telemetry_records([{"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/verify-claim"}])
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 10, base)
        out = run_hook({"session_id": session, "transcript_path": transcript}, td)
        check("backstop stays silent when the oldest fire isn't stale yet", out == "", out)
        check("no sentinel written when it doesn't fire", not os.path.exists(os.path.join(td, f"{session}.grade-backstop-fired")))

    with_temp_verdicts(run)


def test_grade_backstop_skips_when_already_graded():
    def run(td):
        session = "sess-backstop-graded"
        base = 1783200000
        write_telemetry_records([{"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/verify-claim"}])
        write_grade_records([{"session_id": session, "seq": 1, "move_id": "anchor/verify-claim", "correct": True, "effective": True, "grader": "session"}])
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, base)
        plant_observation_prompt_fired(td, session)
        out = run_hook({"session_id": session, "transcript_path": transcript}, td)
        check("backstop stays silent once the fire is graded", out == "", out)

    with_temp_verdicts(run)


def test_grade_backstop_ignores_non_gradeable_move_families():
    def run(td):
        session = "sess-backstop-advice-only"
        base = 1783200000
        write_telemetry_records([
            {"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "mechanical/reasoning-primer"},
            {"ts": base + 1, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 2, "move_id": "phase/no-verify-before-reporting"},
        ])
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, base)
        plant_observation_prompt_fired(td, session)
        out = run_hook({"session_id": session, "transcript_path": transcript}, td)
        check("advice/phase fires never require a self-grade", out == "", out)

    with_temp_verdicts(run)


# ---- observation review prompt (moved from test_stop_valve.py) ----


def test_observation_prompt_stays_silent_on_a_short_session():
    def run(td):
        session = "sess-obs-short"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 5, 1783200000)
        out = run_hook({"session_id": session, "transcript_path": transcript}, td)
        check("a short session (< 40 events) isn't asked yet", out == "", out)
        check("no sentinel written when it doesn't fire", not os.path.exists(os.path.join(td, f"{session}.observation-prompt-fired")))

    with_temp_verdicts(run)


def test_observation_prompt_fires_once_as_additional_context():
    def run(td):
        session = "sess-obs-fire"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, 1783200000)
        out1 = run_hook({"session_id": session, "transcript_path": transcript}, td)
        ctx1, d1 = context_of(out1)
        check("substantial session gets asked via additionalContext", ctx1 is not None, out1)
        check("reason names the observation-prompt move", ctx1 and 'move="mechanical/observation-prompt"' in ctx1, ctx1)
        check("reason says it asks once, not every turn", ctx1 and "once per session" in ctx1, ctx1)
        check("reason never demands a filler record", ctx1 and "no action needed" in ctx1, ctx1)
        check("sentinel written", os.path.exists(os.path.join(td, f"{session}.observation-prompt-fired")))

        out2 = run_hook({"session_id": session, "transcript_path": transcript}, td)
        check("second turn stays silent — one ask per session, no logged record required", out2 == "", out2)

    with_temp_verdicts(run)


def test_grade_backstop_takes_priority_over_observation_prompt():
    def run(td):
        session = "sess-priority"
        base = 1783200000
        write_telemetry_records([{"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/verify-claim"}])
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, base)  # clears both gates at once
        out = run_hook({"session_id": session, "transcript_path": transcript}, td)
        ctx, d = context_of(out)
        check("grade backstop wins when both would qualify this turn", ctx and 'move="mechanical/grade-backstop"' in ctx, out)
        check("observation prompt does not also fire in the same turn", ctx and 'move="mechanical/observation-prompt"' not in ctx, out)

    with_temp_verdicts(run)


def test_pending_mailbox_flag_takes_priority_over_housekeeping():
    def run(td):
        session = "sess-priority-mailbox"
        base = 1783200000
        write_verdict(td, session, "anchor/circling", 1)
        write_telemetry_records([{"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 5, "move_id": "anchor/verify-claim"}])
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, base)
        out = run_hook({"session_id": session, "transcript_path": transcript}, td)
        ctx, d = context_of(out)
        check("a pending mailbox flag wins over either housekeeping nag", ctx and 'move="anchor/circling"' in ctx, out)

    with_temp_verdicts(run)


def test_malformed_stdin_and_missing_session_exit_clean():
    def run(td):
        out1 = run_hook("not json at all {{{", td)
        check("malformed stdin JSON -> silent", out1 == "", out1)
        out2 = run_hook({"transcript_path": "/dev/null"}, td)
        check("missing session_id -> silent", out2 == "", out2)

    with_temp_verdicts(run)


def main():
    tests = [
        test_pending_flag_delivers_as_additional_context,
        test_no_pending_flag_falls_through_to_housekeeping_checks,
        test_session_gradeable_fires_filters_prefix_agent_and_null_seq,
        test_session_grade_count_sums_across_live_grades_files,
        test_events_since_counts_tool_results_strictly_after_ts,
        test_grade_backstop_fires_as_additional_context_when_ungraded_and_stale,
        test_grade_backstop_skips_when_not_stale_enough,
        test_grade_backstop_skips_when_already_graded,
        test_grade_backstop_ignores_non_gradeable_move_families,
        test_observation_prompt_stays_silent_on_a_short_session,
        test_observation_prompt_fires_once_as_additional_context,
        test_grade_backstop_takes_priority_over_observation_prompt,
        test_pending_mailbox_flag_takes_priority_over_housekeeping,
        test_malformed_stdin_and_missing_session_exit_clean,
    ]
    for t in tests:
        t()
    for name in PASS:
        print(f"PASS: {name}")
    for name, detail in FAIL:
        print(f"FAIL: {name} ({detail!r})")
    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
