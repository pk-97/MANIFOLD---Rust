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
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

DAEMON_DIR = Path(__file__).resolve().parent
HOOK_PATH = DAEMON_DIR.parent / "hooks" / "daemon-stop.py"
sys.path.insert(0, str(DAEMON_DIR))
import valve  # noqa: E402

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
    orig_payload_cache = valve._PAYLOAD_CACHE
    with tempfile.TemporaryDirectory() as td:
        valve.VERDICTS_DIR = td
        valve.WORKER_NUDGES_FLAG = os.path.join(td, "worker-nudges.enabled")
        # Sleep pass 1: tests were writing `injected` records into the LIVE
        # telemetry.jsonl (30 sess-* records polluted the graded week).
        valve.TELEMETRY_PATH = os.path.join(td, "telemetry.jsonl")
        valve._PAYLOAD_CACHE = None  # force reload against real moves.md (unpatched path)
        try:
            fn(td)
        finally:
            valve.VERDICTS_DIR = orig_verdicts
            valve.WORKER_NUDGES_FLAG = orig_flag
            valve.TELEMETRY_PATH = orig_telemetry
            valve._PAYLOAD_CACHE = orig_payload_cache


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
