#!/usr/bin/env python3
"""
Standalone test for observer.py's worker-nudges extension (DESIGN.md §2b,
shipped OFF). Covers: (a) the flag gate — _scan_agents is a true no-op when
WORKER_NUDGES_FLAG is absent, (b) agent discovery + per-agent mailbox
delivery when the flag is present, using a synthetic subagent transcript
under a temp dir. Never touches the real verdicts dir or a live classifier
(these tests don't reach a real flag, so `_handle_window` is exercised only
through a monkeypatched `common.call_classifier`).

Run: python3 .claude/daemon/test_worker_nudges.py
"""
import importlib.util
import io
import json
import os
import sys
import tempfile
import time
from pathlib import Path

DAEMON_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(DAEMON_DIR))

spec = importlib.util.spec_from_file_location("observer", DAEMON_DIR / "observer.py")
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)
import common  # noqa: E402
import valve  # noqa: E402

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


def with_temp_dirs(fn):
    orig_verdicts = observer.VERDICTS_DIR
    orig_flag = observer.WORKER_NUDGES_FLAG
    orig_valve_verdicts, orig_valve_flag = valve.VERDICTS_DIR, valve.WORKER_NUDGES_FLAG
    with tempfile.TemporaryDirectory() as td:
        observer.VERDICTS_DIR = td
        observer.WORKER_NUDGES_FLAG = os.path.join(td, "worker-nudges.enabled")
        valve.VERDICTS_DIR = td
        valve.WORKER_NUDGES_FLAG = os.path.join(td, "worker-nudges.enabled")
        try:
            fn(td)
        finally:
            observer.VERDICTS_DIR, observer.WORKER_NUDGES_FLAG = orig_verdicts, orig_flag
            valve.VERDICTS_DIR, valve.WORKER_NUDGES_FLAG = orig_valve_verdicts, orig_valve_flag


def write_agent_transcript(path, task_text, reply_text):
    """A minimal two-line subagent transcript: one user turn setting a task,
    one assistant text reply long enough to close a window."""
    with open(path, "w", encoding="utf-8") as f:
        f.write(json.dumps({"type": "user", "message": {"role": "user", "content": task_text}, "timestamp": "2026-07-04T00:00:00Z"}) + "\n")
        f.write(json.dumps({
            "type": "assistant",
            "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [{"type": "text", "text": reply_text}]},
            "timestamp": "2026-07-04T00:00:01Z",
        }) + "\n")


def test_scan_agents_noop_when_flag_absent():
    def run(td):
        session_dir = os.path.join(td, "session")
        os.makedirs(os.path.join(session_dir, "subagents"))
        write_agent_transcript(
            os.path.join(session_dir, "subagents", "agent-abc123.jsonl"),
            "do the thing please make it work",
            "Here is a sufficiently long reply that would close a window if fed.",
        )
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
        d.session_dir = session_dir
        logf = io.StringIO()
        check("flag absent", not observer._worker_nudges_enabled())
        d._scan_agents(logf)
        check("no-op: flag absent -> no agents discovered", d.agents == {})
    with_temp_dirs(run)


def test_scan_agents_discovers_and_catches_up_when_enabled():
    def run(td):
        with open(observer.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session_dir = os.path.join(td, "session")
        os.makedirs(os.path.join(session_dir, "subagents"))
        write_agent_transcript(
            os.path.join(session_dir, "subagents", "agent-abc123.jsonl"),
            "do the thing please make it work",
            "Here is a sufficiently long reply that would close a window if fed.",
        )
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
        d.session_dir = session_dir
        logf = io.StringIO()
        d._scan_agents(logf)
        check("enabled: agent discovered", "abc123" in d.agents)
        worker = d.agents.get("abc123")
        check("worker task captured from catchup", worker and worker.state.current_task and "do the thing" in worker.state.current_task, getattr(worker, "state", None) and worker.state.current_task)
        check("worker mailbox path uses <session>.<agent_id> key", worker and worker.verdict_path.endswith("sess1.abc123.json"), worker and worker.verdict_path)
    with_temp_dirs(run)


def test_agent_window_closes_and_classifies_independently():
    def run(td):
        with open(observer.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session_dir = os.path.join(td, "session")
        os.makedirs(os.path.join(session_dir, "subagents"))
        agent_path = os.path.join(session_dir, "subagents", "agent-xyz789.jsonl")
        write_agent_transcript(agent_path, "investigate the failing widget test", "still looking into it")
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
        d.session_dir = session_dir

        calls = []

        def fake_classifier(system_prompt, window_text, *a, **kw):
            calls.append(window_text)
            return {"phase": "verifying", "flag": None}

        orig = common.call_classifier
        observer.common.call_classifier = fake_classifier
        try:
            logf = io.StringIO()
            d._scan_agents(logf)  # catchup only, no classify yet (only 1 window, closes on 2nd text)

            # Append a second assistant text -> closes a window for classify=True path.
            with open(agent_path, "a", encoding="utf-8") as f:
                f.write(json.dumps({
                    "type": "assistant",
                    "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [{"type": "text", "text": "a second, longer reply that should close another window here"}]},
                    "timestamp": "2026-07-04T00:00:02Z",
                }) + "\n")
            d._scan_agents(logf)

            check("agent window classified independently (fake classifier called)", len(calls) >= 1, calls)
            worker = d.agents["xyz789"]
            check("agent verdict file written", os.path.exists(worker.verdict_path))
            check("session-level verdict file untouched by agent activity", not os.path.exists(d.verdict_path))
        finally:
            observer.common.call_classifier = orig
    with_temp_dirs(run)


def test_session_mailbox_unaffected_by_agent_activity():
    """Regression guard: the main session's own fire-tracking dicts and
    verdict file must be untouched by agent scanning."""
    def run(td):
        with open(observer.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session_dir = os.path.join(td, "session")
        os.makedirs(os.path.join(session_dir, "subagents"))
        write_agent_transcript(
            os.path.join(session_dir, "subagents", "agent-def456.jsonl"),
            "some agent task",
            "some agent reply that is long enough to close a window",
        )
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
        d.session_dir = session_dir
        logf = io.StringIO()
        d._scan_agents(logf)
        check("session fire_count untouched", d.fire_count == {})
        check("session last_fire_event untouched", d.last_fire_event == {})
        check("session next_seq untouched (still 1)", d.next_seq == 1)
    with_temp_dirs(run)


def test_posttooluse_hook_agent_event_quiet_without_planted_verdict():
    """End-to-end against the REAL repo state (deliberately not sandboxed —
    the hook subprocess imports its own fresh `valve`, reading the real
    verdicts dir): an agent_id-tagged event with no verdict planted for that
    agent's mailbox must make the real hook print nothing. Holds in BOTH flag
    states: flag absent = agent events skipped; flag present (the live state
    since Peter enabled worker nudges 2026-07-04, DESIGN §2b) = agent mailbox
    consulted, found empty, silent. The old ship-dark invariant assertion was
    removed when enablement became the shipped state."""
    import subprocess
    real_verdicts = os.path.join(DAEMON_DIR, "verdicts")
    fake_session = "test-session-for-hook-probe"
    # Pre-claim a pidfile with OUR OWN (guaranteed-alive) pid so the hook's
    # ensure_observer() sees an already-running daemon and does not spawn a
    # real observer.py subprocess against a throwaway session_id.
    fake_pidfile = os.path.join(real_verdicts, f"{fake_session}.pid")
    os.makedirs(real_verdicts, exist_ok=True)
    with open(fake_pidfile, "w", encoding="utf-8") as f:
        f.write(str(os.getpid()))
    try:
        payload = json.dumps({
            "tool_name": "Bash",
            "session_id": fake_session,
            "agent_id": "some-agent-id",
            "transcript_path": "/dev/null",
        })
        hook_path = DAEMON_DIR.parent / "hooks" / "daemon-posttooluse.py"
        r = subprocess.run([sys.executable, str(hook_path)], input=payload, capture_output=True, text=True)
        check("hook prints nothing for agent_id with empty mailbox", r.stdout.strip() == "", r.stdout)
    finally:
        for suffix in (".pid", ".json", ".consumed", ".log"):
            try:
                os.remove(os.path.join(real_verdicts, f"{fake_session}{suffix}"))
            except OSError:
                pass


def main():
    tests = [
        test_scan_agents_noop_when_flag_absent,
        test_scan_agents_discovers_and_catches_up_when_enabled,
        test_agent_window_closes_and_classifies_independently,
        test_session_mailbox_unaffected_by_agent_activity,
        test_posttooluse_hook_agent_event_quiet_without_planted_verdict,
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
