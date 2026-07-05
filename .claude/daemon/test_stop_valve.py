#!/usr/bin/env python3
"""
Standalone test for the Stop-hook valve (daemon-stop.py), DESIGN.md §2
"Known delivery gap" + moves.md mechanical/announced-not-started.

Runs the real hook as a subprocess against synthetic stdin and planted
verdict/transcript files under a temp VERDICTS_DIR (monkeypatched into
`valve` for the in-process helper calls, and passed via env for the
subprocess — see `run_hook`). Never touches the real verdicts dir.

Run: python3 .claude/daemon/test_stop_valve.py
"""
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

DAEMON_DIR = Path(__file__).resolve().parent
HOOK_PATH = DAEMON_DIR.parent / "hooks" / "daemon-stop.py"
sys.path.insert(0, str(DAEMON_DIR))
import valve  # noqa: E402

# A second, persistently-loaded copy of the hook module for testing the
# grade-backstop helper functions directly (2026-07-05 addressability fix) —
# `run_hook` below deliberately reloads a FRESH module per call (see its own
# docstring), which is right for full end-to-end runs but leaves no handle
# to call a helper in isolation. This is a separate module object; nothing
# it does affects run_hook's own fresh-load behavior.
_direct_spec = importlib.util.spec_from_file_location("daemon_stop_direct", HOOK_PATH)
DIRECT = importlib.util.module_from_spec(_direct_spec)
_direct_spec.loader.exec_module(DIRECT)

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


def run_hook(stdin_obj, verdicts_dir):
    """Run the real daemon-stop.py as a subprocess with VERDICTS_DIR
    redirected via monkeypatch shim: the hook imports valve fresh in its own
    process, so we point it at our temp dir by writing a tiny sitecustomize-
    style override isn't available — instead we patch valve.py's module
    globals via an env-var-driven shim would require editing valve.py, which
    this test must not do. Simplest correct approach: run the hook in-process
    by exec'ing its main() with sys.path already pointed at DAEMON_DIR, after
    monkeypatching `valve.VERDICTS_DIR`/`valve.WORKER_NUDGES_FLAG` for the
    imported valve module the hook itself will import (same module object,
    since Python caches by sys.path + name)."""
    import importlib.util

    spec = importlib.util.spec_from_file_location("daemon_stop", HOOK_PATH)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)

    orig_stdin = sys.stdin
    orig_stdout = sys.stdout
    import io

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
    orig_flag = valve.WORKER_NUDGES_FLAG
    orig_telemetry = valve.TELEMETRY_PATH
    orig_eval_dir = valve.EVAL_DIR
    orig_payload_cache = valve._PAYLOAD_CACHE
    with tempfile.TemporaryDirectory() as td:
        valve.VERDICTS_DIR = td
        valve.WORKER_NUDGES_FLAG = os.path.join(td, "worker-nudges.enabled")
        # Sleep pass 1: tests were writing `injected` records into the LIVE
        # telemetry.jsonl (30 sess-* records polluted the graded week).
        valve.TELEMETRY_PATH = os.path.join(td, "telemetry.jsonl")
        # Grade-backstop tests (2026-07-05) need their own eval/ dir — never
        # the real .claude/daemon/eval/live_grades*.jsonl.
        valve.EVAL_DIR = os.path.join(td, "eval")
        valve._PAYLOAD_CACHE = None  # force reload against real moves.md (unpatched path)
        try:
            fn(td)
        finally:
            valve.VERDICTS_DIR = orig_verdicts
            valve.WORKER_NUDGES_FLAG = orig_flag
            valve.TELEMETRY_PATH = orig_telemetry
            valve.EVAL_DIR = orig_eval_dir
            valve._PAYLOAD_CACHE = orig_payload_cache


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
    """A transcript with `n_events` tool_result-bearing user records at
    1-second intervals starting just after `start_ts`, followed by a final
    assistant text that is NOT an announcement (so _announced_not_started
    stays quiet and the grade-backstop check is reached)."""

    def iso(ts):
        return datetime.fromtimestamp(ts, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    lines = []
    for i in range(n_events):
        lines.append(
            json.dumps(
                {
                    "type": "user",
                    "timestamp": iso(start_ts + i + 1),
                    "message": {
                        "role": "user",
                        "content": [{"type": "tool_result", "tool_use_id": f"t{i}", "content": "ok"}],
                    },
                }
            )
        )
    lines.append(
        json.dumps(
            {
                "type": "assistant",
                "timestamp": iso(start_ts + n_events + 1),
                "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [{"type": "text", "text": final_text}]},
            }
        )
    )
    with open(path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")


def write_verdict(verdicts_dir, key, move_id, seq, ts=None):
    path = os.path.join(verdicts_dir, f"{key}.json")
    with open(path, "w", encoding="utf-8") as f:
        json.dump(
            {
                "ts": ts if ts is not None else time.time(),
                "flag": {"move_id": move_id, "seq": seq, "evidence": "test", "confidence": 0.9},
            },
            f,
        )


def write_transcript(path, final_text, tool_use_after_text=False):
    """A minimal transcript whose LAST line is a single assistant message.
    If tool_use_after_text, a tool_use block follows the text block within
    that same message (so the mechanical check must NOT fire)."""
    content = [{"type": "text", "text": final_text}]
    if tool_use_after_text:
        content.append({"type": "tool_use", "id": "tu1", "name": "Read", "input": {"file_path": "x"}})
    with open(path, "w", encoding="utf-8") as f:
        f.write(json.dumps({"type": "user", "message": {"role": "user", "content": "do the thing"}, "timestamp": "2026-07-04T00:00:00Z"}) + "\n")
        f.write(json.dumps({
            "type": "assistant",
            "message": {"role": "assistant", "model": "claude-sonnet-5", "content": content},
            "timestamp": "2026-07-04T00:00:01Z",
        }) + "\n")


def test_pending_flag_blocks_once_and_sentinel_guards_repeat():
    def run(td):
        session = "sess-pending"
        write_verdict(td, session, "anchor/verify-claim", seq=1)
        out1 = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": "/dev/null"}, td)
        d1 = json.loads(out1) if out1 else None
        check("first call blocks", d1 is not None and d1.get("decision") == "block", out1)
        check("reason carries the move tag", d1 and 'move="anchor/verify-claim"' in d1.get("reason", ""), d1)
        check("sentinel file written", os.path.exists(os.path.join(td, f"{session}.stopblock.p1")))
        check("consumed marker written", valve.read_consumed(session) == 1)

        # A fresh, higher-seq flag lands (as if the observer fired again),
        # but the SAME prompt_id already spent its block this turn.
        write_verdict(td, session, "anchor/verify-claim", seq=2)
        out2 = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": "/dev/null"}, td)
        check("second call for same prompt_id is silent (sentinel guard)", out2 == "", out2)
    with_temp_verdicts(run)


def test_stop_hook_active_short_circuits():
    def run(td):
        session = "sess-active"
        write_verdict(td, session, "anchor/verify-claim", seq=1)
        out = run_hook({"session_id": session, "prompt_id": "p1", "stop_hook_active": True, "transcript_path": "/dev/null"}, td)
        check("stop_hook_active suppresses block even with pending flag", out == "", out)
        check("no sentinel written when stop_hook_active short-circuits", not os.path.exists(os.path.join(td, f"{session}.stopblock.p1")))
    with_temp_verdicts(run)


def test_announcement_ending_blocks_with_mechanical_payload():
    def run(td):
        session = "sess-announce"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "I found the issue. Starting the migration now with the new schema.")
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check("announcement ending blocks", d is not None and d.get("decision") == "block", out)
        check("reason carries mechanical move id", d and 'move="mechanical/announced-not-started"' in d.get("reason", ""), d)
    with_temp_verdicts(run)


def test_beginning_and_let_me_now_variants_fire():
    def run(td):
        for i, text in enumerate(["Beginning the refactor across the crate.", "Let me now build the harness."]):
            session = f"sess-variant-{i}"
            transcript = os.path.join(td, f"t{i}.jsonl")
            write_transcript(transcript, text)
            out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
            d = json.loads(out) if out else None
            check(f"variant fires: {text!r}", d is not None and d.get("decision") == "block", out)
    with_temp_verdicts(run)


def test_tool_use_after_text_does_not_fire():
    def run(td):
        session = "sess-tooluse-after"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Starting the migration now.", tool_use_after_text=True)
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("tool_use after the text suppresses the mechanical fire", out == "", out)
    with_temp_verdicts(run)


def test_handoff_and_question_endings_do_not_fire():
    def run(td):
        cases = [
            "I'll do this once you confirm the approach.",
            "Should I start the migration now?",
            "Next session I'll pick this up.",
        ]
        for i, text in enumerate(cases):
            session = f"sess-noannounce-{i}"
            transcript = os.path.join(td, f"t{i}.jsonl")
            write_transcript(transcript, text)
            out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
            check(f"non-announcement stays silent: {text!r}", out == "", out)
    with_temp_verdicts(run)


def test_agent_id_routes_to_agent_mailbox():
    def run(td):
        session, agent = "sess-agent", "abc123"
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        write_verdict(td, f"{session}.{agent}", "anchor/circling", seq=1)
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": "/dev/null"}, td)
        d = json.loads(out) if out else None
        check("agent-tagged Stop reads the agent's own mailbox", d is not None and d.get("decision") == "block", out)
        check("agent mailbox consumed marker written under session.agent key", valve.read_consumed(f"{session}.{agent}") == 1)
        check("session-level mailbox untouched", valve.read_consumed(session) == 0)
    with_temp_verdicts(run)


def test_agent_id_silent_when_worker_nudges_disabled():
    def run(td):
        session, agent = "sess-agent-off", "def456"
        write_verdict(td, f"{session}.{agent}", "anchor/circling", seq=1)
        check("worker-nudges flag absent in this temp dir", not valve.worker_nudges_enabled())
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": "/dev/null"}, td)
        check("agent-tagged Stop stays dark without the flag", out == "", out)
    with_temp_verdicts(run)


def test_malformed_stdin_and_missing_transcript_exit_clean():
    def run(td):
        out1 = run_hook("not json at all {{{", td)
        check("malformed stdin JSON -> silent", out1 == "", out1)
        out2 = run_hook({"session_id": "sess-no-transcript", "prompt_id": "p1", "transcript_path": "/no/such/file.jsonl"}, td)
        check("missing transcript file -> silent", out2 == "", out2)
        out3 = run_hook({"prompt_id": "p1"}, td)
        check("missing session_id -> silent", out3 == "", out3)
    with_temp_verdicts(run)


def test_stale_stopblock_sentinel_is_swept():
    def run(td):
        session = "sess-sweep"
        stale = os.path.join(td, f"{session}.stopblock.old")
        open(stale, "w").close()
        old_time = time.time() - (25 * 60 * 60)
        os.utime(stale, (old_time, old_time))
        # Any invocation triggers the sweep, even one with nothing to block.
        run_hook({"session_id": "sess-unrelated", "prompt_id": "p9", "transcript_path": "/dev/null"}, td)
        check("stale sentinel (>1 day) removed by the sweep", not os.path.exists(stale))
    with_temp_verdicts(run)


def test_real_hook_process_smoke():
    """One true end-to-end run through the actual subprocess (not the
    in-process exec used by the rest of this file), against a throwaway
    session id in the REAL repo verdicts dir, to catch anything the
    in-process harness's module reload could paper over (import errors,
    syntax errors, path resolution)."""
    fake_session = "test-session-for-stop-hook-smoke"
    real_verdicts = os.path.join(DAEMON_DIR, "verdicts")
    os.makedirs(real_verdicts, exist_ok=True)
    payload = json.dumps({"session_id": fake_session, "prompt_id": "p1", "transcript_path": "/dev/null"})
    r = subprocess.run([sys.executable, str(HOOK_PATH)], input=payload, capture_output=True, text=True)
    check("real subprocess exits 0", r.returncode == 0, r.returncode)
    check("real subprocess prints nothing for a session with no verdict", r.stdout.strip() == "", r.stdout)
    for suffix in (".json", ".consumed", f".stopblock.p1"):
        try:
            os.remove(os.path.join(real_verdicts, f"{fake_session}{suffix}"))
        except OSError:
            pass


# ---- catch-up Stop-wait (re-ruled 2026-07-05: wait for the observer's
# .offset heartbeat to reach the transcript size at Stop, instead of the
# old classifying-marker race) ----


def plant_observer(td, session, drained_offset):
    """A 'live' observer for the hook's liveness gate: a pid file holding
    OUR pid (always alive) plus a heartbeat at the given offset."""
    with open(os.path.join(td, f"{session}.pid"), "w", encoding="utf-8") as f:
        f.write(str(os.getpid()))
    with open(os.path.join(td, f"{session}.offset"), "w", encoding="utf-8") as f:
        f.write(str(drained_offset))


def plant_observation_prompt_fired(td, session):
    """Pre-consume the (unrelated) observation-review-prompt's one-shot
    sentinel so a test can exercise another Stop mechanism in isolation on a
    long (>=40-event) transcript without the prompt also firing."""
    open(os.path.join(td, f"{session}.observation-prompt-fired"), "w").close()


def test_stop_wait_delivers_verdict_when_observer_catches_up():
    def run(td):
        session = "sess-catchup"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "The fix is in and the tests pass.")
        size = os.path.getsize(transcript)
        plant_observer(td, session, 0)  # observer alive but behind
        # Simulate the observer draining the turn-final text 0.5s in: verdict
        # first (written inside the drain), then the heartbeat catches up.
        import threading

        def observer_drains():
            time.sleep(0.5)
            write_verdict(td, session, "anchor/verify-claim", seq=1)
            with open(os.path.join(td, f"{session}.offset"), "w", encoding="utf-8") as f:
                f.write(str(size))

        t = threading.Thread(target=observer_drains)
        t.start()
        start = time.time()
        out = run_hook({"session_id": session, "prompt_id": "pw1", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        t.join()
        d = json.loads(out) if out else None
        check("catch-up wait delivered the landing verdict", d and d.get("decision") == "block", out)
        check("catch-up wait was short", elapsed < 3.0, elapsed)

    with_temp_verdicts(run)


def test_stop_wait_fast_when_observer_already_caught_up():
    def run(td):
        session = "sess-caughtup"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Here is the summary of what I found.")
        plant_observer(td, session, os.path.getsize(transcript))
        start = time.time()
        out = run_hook({"session_id": session, "prompt_id": "pw2", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        check("caught-up observer: no wait", elapsed < 0.5, elapsed)
        check("caught-up observer: no block", not out, out)

    with_temp_verdicts(run)


def test_stop_wait_skips_when_observer_dead_or_no_heartbeat():
    def run(td):
        # Dead observer (no pid file) with a lagging heartbeat: no wait.
        session = "sess-deadobs"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Here is the summary of what I found.")
        with open(os.path.join(td, f"{session}.offset"), "w", encoding="utf-8") as f:
            f.write("0")
        start = time.time()
        out = run_hook({"session_id": session, "prompt_id": "pw3", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        check("dead observer: no wait", elapsed < 0.5, elapsed)
        check("dead observer: no block", not out, out)

        # Live pid but no heartbeat file (pre-heartbeat observer): no wait.
        session2 = "sess-nohb"
        with open(os.path.join(td, f"{session2}.pid"), "w", encoding="utf-8") as f:
            f.write(str(os.getpid()))
        start = time.time()
        out2 = run_hook({"session_id": session2, "prompt_id": "pw4", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        check("missing heartbeat: no wait", elapsed < 0.5, elapsed)
        check("missing heartbeat: no block", not out2, out2)

    with_temp_verdicts(run)


def test_stop_wait_bounded_when_observer_stalls():
    def run(td):
        session = "sess-stall"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Here is the summary of what I found.")
        plant_observer(td, session, 0)  # alive, heartbeat never moves
        start = time.time()
        out = run_hook({"session_id": session, "prompt_id": "pw5", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        check("stalled observer: returns within cap + slack", elapsed < 7.5, elapsed)
        check("stalled observer: waited to the cap, not less", elapsed > 5.0, elapsed)
        check("stalled observer: no block", not out, out)

    with_temp_verdicts(run)


def test_stop_wait_never_runs_for_workers():
    def run(td):
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session, agent = "sess-waitagent", "agw1"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Here is the summary of what I found.")
        plant_observer(td, session, 0)  # lagging heartbeat must not matter
        start = time.time()
        out = run_hook(
            {"session_id": session, "agent_id": agent, "prompt_id": "pw6", "transcript_path": transcript}, td
        )
        elapsed = time.time() - start
        check("worker stop never waits", elapsed < 0.5, elapsed)
        check("worker stop no block", not out, out)

    with_temp_verdicts(run)


# ---- grade-backstop (2026-07-05 review): sessions never write self-grades
# at all, so the sleep pass has nothing to join fires against. Direct helper
# tests first, then full hook runs. ----


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


def test_grade_backstop_fires_when_ungraded_and_stale():
    def run(td):
        session = "sess-backstop-fire"
        base = 1783200000
        write_telemetry_records(
            [{"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/verify-claim"}]
        )
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, base)
        out1 = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d1 = json.loads(out1) if out1 else None
        check("backstop blocks when stale and ungraded", d1 is not None and d1.get("decision") == "block", out1)
        check("reason names the backstop move", d1 and 'move="mechanical/grade-backstop"' in d1.get("reason", ""), d1)
        check("reason states the ungraded count", d1 and "1 gradeable" in d1.get("reason", ""), d1)
        check("session-level sentinel written", os.path.exists(os.path.join(td, f"{session}.grade-backstop-fired")))

        # Isolate: the (unrelated) observation-review prompt also clears its
        # own >=40-event gate on this transcript — pre-consume its sentinel
        # so this assertion is about the grade backstop alone.
        plant_observation_prompt_fired(td, session)
        out2 = run_hook({"session_id": session, "prompt_id": "p2", "transcript_path": transcript}, td)
        check("second turn stays silent (once per session, not per turn)", out2 == "", out2)

    with_temp_verdicts(run)


def test_grade_backstop_skips_when_not_stale_enough():
    def run(td):
        session = "sess-backstop-fresh"
        base = 1783200000
        write_telemetry_records(
            [{"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/verify-claim"}]
        )
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 10, base)
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("backstop stays silent when the oldest fire isn't stale yet", out == "", out)
        check("no sentinel written when it doesn't fire", not os.path.exists(os.path.join(td, f"{session}.grade-backstop-fired")))

    with_temp_verdicts(run)


def test_grade_backstop_skips_when_already_graded():
    def run(td):
        session = "sess-backstop-graded"
        base = 1783200000
        write_telemetry_records(
            [{"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/verify-claim"}]
        )
        write_grade_records([{"session_id": session, "seq": 1, "move_id": "anchor/verify-claim", "correct": True, "effective": True, "grader": "session"}])
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, base)
        plant_observation_prompt_fired(td, session)  # isolate: unrelated 40-event gate
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("backstop stays silent once the fire is graded", out == "", out)

    with_temp_verdicts(run)


def test_grade_backstop_never_fires_for_worker_stop():
    def run(td):
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session, agent = "sess-backstop-worker", "wk1"
        base = 1783200000
        write_telemetry_records(
            [{"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/verify-claim"}]
        )
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, base)
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": transcript}, td)
        check("worker-tagged Stop never triggers the main-session grade backstop", out == "", out)

    with_temp_verdicts(run)


def test_grade_backstop_ignores_non_gradeable_move_families():
    def run(td):
        session = "sess-backstop-advice-only"
        base = 1783200000
        write_telemetry_records(
            [
                {"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "mechanical/reasoning-primer"},
                {"ts": base + 1, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 2, "move_id": "phase/no-verify-before-reporting"},
            ]
        )
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, base)
        plant_observation_prompt_fired(td, session)  # isolate: unrelated 40-event gate
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("advice/phase fires never require a self-grade", out == "", out)

    with_temp_verdicts(run)


# ---- observation review prompt (Peter, 2026-07-05): a standing invitation
# to log anything worth the next sleep pass's attention. Fires once per
# session, only once the session has done enough to be worth reviewing —
# never a repeated demand, since most sessions have nothing to add. ----


def test_observation_prompt_stays_silent_on_a_short_session():
    def run(td):
        session = "sess-obs-short"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 5, 1783200000)
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("a short session (< 40 events) isn't asked yet", out == "", out)
        check("no sentinel written when it doesn't fire", not os.path.exists(os.path.join(td, f"{session}.observation-prompt-fired")))

    with_temp_verdicts(run)


def test_observation_prompt_fires_once_on_a_substantial_session():
    def run(td):
        session = "sess-obs-fire"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, 1783200000)
        out1 = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d1 = json.loads(out1) if out1 else None
        check("substantial session gets asked", d1 is not None and d1.get("decision") == "block", out1)
        check("reason names the observation-prompt move", d1 and 'move="mechanical/observation-prompt"' in d1.get("reason", ""), d1)
        check("reason says it asks once, not every turn", d1 and "once per session" in d1.get("reason", ""), d1)
        check("reason never demands a filler record", d1 and "no action needed" in d1.get("reason", ""), d1)
        check("sentinel written", os.path.exists(os.path.join(td, f"{session}.observation-prompt-fired")))

        out2 = run_hook({"session_id": session, "prompt_id": "p2", "transcript_path": transcript}, td)
        check("second turn stays silent — one ask per session, no logged record required", out2 == "", out2)

    with_temp_verdicts(run)


def test_observation_prompt_never_fires_for_worker_stop():
    def run(td):
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session, agent = "sess-obs-worker", "wk1"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, 1783200000)
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": transcript}, td)
        check("worker-tagged Stop never triggers the main-session observation prompt", out == "", out)

    with_temp_verdicts(run)


def main():
    tests = [
        test_pending_flag_blocks_once_and_sentinel_guards_repeat,
        test_stop_hook_active_short_circuits,
        test_announcement_ending_blocks_with_mechanical_payload,
        test_beginning_and_let_me_now_variants_fire,
        test_tool_use_after_text_does_not_fire,
        test_handoff_and_question_endings_do_not_fire,
        test_agent_id_routes_to_agent_mailbox,
        test_agent_id_silent_when_worker_nudges_disabled,
        test_malformed_stdin_and_missing_transcript_exit_clean,
        test_stale_stopblock_sentinel_is_swept,
        test_real_hook_process_smoke,
        test_stop_wait_delivers_verdict_when_observer_catches_up,
        test_stop_wait_fast_when_observer_already_caught_up,
        test_stop_wait_skips_when_observer_dead_or_no_heartbeat,
        test_stop_wait_bounded_when_observer_stalls,
        test_stop_wait_never_runs_for_workers,
        test_session_gradeable_fires_filters_prefix_agent_and_null_seq,
        test_session_grade_count_sums_across_live_grades_files,
        test_events_since_counts_tool_results_strictly_after_ts,
        test_grade_backstop_fires_when_ungraded_and_stale,
        test_grade_backstop_skips_when_not_stale_enough,
        test_grade_backstop_skips_when_already_graded,
        test_grade_backstop_never_fires_for_worker_stop,
        test_grade_backstop_ignores_non_gradeable_move_families,
        test_observation_prompt_stays_silent_on_a_short_session,
        test_observation_prompt_fires_once_on_a_substantial_session,
        test_observation_prompt_never_fires_for_worker_stop,
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
