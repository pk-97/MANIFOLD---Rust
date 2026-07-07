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
        # Real layout: subagent transcripts live in <project>/<session_id>/subagents/,
        # and the Daemon derives that path itself — no attribute override here.
        # (The old override is what let the wrong derivation ship unnoticed.)
        session_dir = os.path.join(td, "session")
        os.makedirs(os.path.join(session_dir, "sess1", "subagents"))
        write_agent_transcript(
            os.path.join(session_dir, "sess1", "subagents", "agent-abc123.jsonl"),
            "do the thing please make it work",
            "Here is a sufficiently long reply that would close a window if fed.",
        )
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
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
        os.makedirs(os.path.join(session_dir, "sess1", "subagents"))
        write_agent_transcript(
            os.path.join(session_dir, "sess1", "subagents", "agent-abc123.jsonl"),
            "do the thing please make it work",
            "Here is a sufficiently long reply that would close a window if fed.",
        )
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
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
        os.makedirs(os.path.join(session_dir, "sess1", "subagents"))
        agent_path = os.path.join(session_dir, "sess1", "subagents", "agent-xyz789.jsonl")
        write_agent_transcript(agent_path, "investigate the failing widget test", "still looking into it")
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))

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
        os.makedirs(os.path.join(session_dir, "sess1", "subagents"))
        write_agent_transcript(
            os.path.join(session_dir, "sess1", "subagents", "agent-def456.jsonl"),
            "some agent task",
            "some agent reply that is long enough to close a window",
        )
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
        logf = io.StringIO()
        d._scan_agents(logf)
        check("session fire_count untouched", d.fire_count == {})
        check("session last_fire_event untouched", d.last_fire_event == {})
        check("session next_seq untouched (still 1)", d.next_seq == 1)
    with_temp_dirs(run)


def test_scan_agents_discovers_workflow_dir_agent_when_enabled():
    """DESIGN.md §2h.3: workflow runs live one directory level deeper —
    <subagents_dir>/workflows/<run_id>/agent-*.jsonl. Real nested layout,
    no session_dir override, same discipline as the top-level test above."""
    def run(td):
        with open(observer.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session_dir = os.path.join(td, "session")
        run_dir = os.path.join(session_dir, "sess1", "subagents", "workflows", "run-001")
        os.makedirs(run_dir)
        write_agent_transcript(
            os.path.join(run_dir, "agent-wf001.jsonl"),
            "run stage one of the workflow",
            "Here is a sufficiently long reply that would close a window if fed.",
        )
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
        logf = io.StringIO()
        d._scan_agents(logf)
        check("workflow-dir agent discovered", "wf001" in d.agents, sorted(d.agents))
        worker = d.agents.get("wf001")
        check(
            "workflow agent mailbox uses the same <session>.<agent_id> key",
            worker and worker.verdict_path.endswith("sess1.wf001.json"),
            worker and worker.verdict_path,
        )
        check(
            "workflow agent task captured from catchup",
            worker and worker.state.current_task and "run stage one" in worker.state.current_task,
            getattr(worker, "state", None) and worker.state.current_task,
        )
    with_temp_dirs(run)


def test_scan_agents_discovers_top_level_and_workflow_agents_together():
    """A session can have BOTH a plain Agent-tool worker and a workflow run
    live at once — the flat self.agents dict must hold both without one
    masking the other."""
    def run(td):
        with open(observer.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session_dir = os.path.join(td, "session")
        subagents_dir = os.path.join(session_dir, "sess1", "subagents")
        run_dir = os.path.join(subagents_dir, "workflows", "run-001")
        os.makedirs(run_dir)
        write_agent_transcript(
            os.path.join(subagents_dir, "agent-top1.jsonl"),
            "top-level agent task", "top-level agent reply, long enough to close a window here",
        )
        write_agent_transcript(
            os.path.join(run_dir, "agent-wf001.jsonl"),
            "workflow agent task", "workflow agent reply, long enough to close a window here",
        )
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
        logf = io.StringIO()
        d._scan_agents(logf)
        check("top-level agent discovered", "top1" in d.agents, sorted(d.agents))
        check("workflow agent discovered", "wf001" in d.agents, sorted(d.agents))
        check("exactly two agents tracked, no cross-talk", len(d.agents) == 2, sorted(d.agents))
    with_temp_dirs(run)


def test_scan_agents_finds_workflow_run_dir_appearing_after_first_scan():
    """DESIGN.md §2h.3: "workflow run directories appear mid-session" — a
    workflow launched well after the observer started must be picked up on
    the NEXT poll, not require a restart. First scan sees only a top-level
    agent (no workflows/ dir exists yet); the run dir is created afterward;
    a second scan must discover it."""
    def run(td):
        with open(observer.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session_dir = os.path.join(td, "session")
        subagents_dir = os.path.join(session_dir, "sess1", "subagents")
        os.makedirs(subagents_dir)
        write_agent_transcript(
            os.path.join(subagents_dir, "agent-early1.jsonl"),
            "an early plain agent", "a reply long enough to close a window right here",
        )
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
        logf = io.StringIO()
        d._scan_agents(logf)
        check("only the early agent is known before the workflow launches", sorted(d.agents) == ["early1"], sorted(d.agents))

        # The workflow launches later: its run directory appears mid-session.
        run_dir = os.path.join(subagents_dir, "workflows", "run-002")
        os.makedirs(run_dir)
        write_agent_transcript(
            os.path.join(run_dir, "agent-late1.jsonl"),
            "a late workflow agent", "a reply long enough to close a window right here too",
        )
        d._scan_agents(logf)
        check(
            "the newly-appeared workflow run dir is discovered on the next poll",
            "late1" in d.agents,
            sorted(d.agents),
        )
        check("the early agent is still tracked too", "early1" in d.agents, sorted(d.agents))
    with_temp_dirs(run)


def test_scan_agents_ignores_non_agent_files_in_workflow_dir():
    def run(td):
        with open(observer.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session_dir = os.path.join(td, "session")
        run_dir = os.path.join(session_dir, "sess1", "subagents", "workflows", "run-001")
        os.makedirs(run_dir)
        with open(os.path.join(run_dir, "meta.json"), "w", encoding="utf-8") as f:
            f.write("{}")
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
        logf = io.StringIO()
        d._scan_agents(logf)
        check("non agent-*.jsonl files in a workflow run dir are ignored", d.agents == {})
    with_temp_dirs(run)


def test_scan_agents_tolerates_missing_workflows_dir():
    """No workflow has ever run this session -> workflows/ doesn't exist at
    all. Must not raise, and must not disturb ordinary top-level discovery."""
    def run(td):
        with open(observer.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session_dir = os.path.join(td, "session")
        os.makedirs(os.path.join(session_dir, "sess1", "subagents"))
        write_agent_transcript(
            os.path.join(session_dir, "sess1", "subagents", "agent-only1.jsonl"),
            "just a plain agent, no workflows involved",
            "a reply long enough to close a window right here",
        )
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
        logf = io.StringIO()
        d._scan_agents(logf)  # must not raise despite no workflows/ dir existing
        check("top-level discovery unaffected by an absent workflows/ dir", "only1" in d.agents, sorted(d.agents))
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


def test_posttooluse_hook_agent_delivery_includes_agent_id_in_ack():
    """DESIGN.md §2h.4: build_block's supervised ack must reach worker
    deliveries through PostToolUse too (the main worker-delivery channel —
    Stop only fires once per turn) with the worker's own agent_id folded
    into the grade-line instruction. Deliberately not sandboxed (matches
    test_posttooluse_hook_agent_event_quiet_without_planted_verdict just
    above): the hook subprocess imports its own fresh `valve`, reading the
    real verdicts dir, so a verdict is planted there directly."""
    import subprocess

    real_verdicts = os.path.join(DAEMON_DIR, "verdicts")
    fake_session = "test-session-for-hook-agent-ack"
    fake_agent = "agent-ack-probe"
    os.makedirs(real_verdicts, exist_ok=True)
    fake_pidfile = os.path.join(real_verdicts, f"{fake_session}.pid")
    with open(fake_pidfile, "w", encoding="utf-8") as f:
        f.write(str(os.getpid()))
    mailbox_key = f"{fake_session}.{fake_agent}"
    verdict_path = os.path.join(real_verdicts, f"{mailbox_key}.json")
    with open(verdict_path, "w", encoding="utf-8") as f:
        json.dump({"ts": time.time(), "flag": {"move_id": "anchor/circling", "seq": 1, "evidence": "test", "confidence": 0.9}}, f)
    # The real repo ships worker-nudges OFF by default (nobody creates this
    # sentinel) — flip it on for just this call, then remove it, so this
    # test doesn't leave the flag enabled for whatever else touches the real
    # verdicts dir afterward. Only remove it if WE created it.
    real_flag_path = os.path.join(real_verdicts, "worker-nudges.enabled")
    we_created_flag = not os.path.exists(real_flag_path)
    if we_created_flag:
        with open(real_flag_path, "w", encoding="utf-8") as f:
            f.write("1")
    try:
        payload = json.dumps({
            "tool_name": "Bash",
            "session_id": fake_session,
            "agent_id": fake_agent,
            "transcript_path": "/dev/null",
        })
        hook_path = DAEMON_DIR.parent / "hooks" / "daemon-posttooluse.py"
        r = subprocess.run([sys.executable, str(hook_path)], input=payload, capture_output=True, text=True)
        check("real subprocess exits 0", r.returncode == 0, r.returncode)
        out = json.loads(r.stdout) if r.stdout.strip() else None
        ctx = (out or {}).get("hookSpecificOutput", {}).get("additionalContext", "")
        check("worker delivery via PostToolUse carries this agent_id in the ack", f'"agent_id": "{fake_agent}"' in ctx, ctx)
    finally:
        if we_created_flag:
            try:
                os.remove(real_flag_path)
            except OSError:
                pass
        for suffix in (".pid", ".log"):
            try:
                os.remove(os.path.join(real_verdicts, f"{fake_session}{suffix}"))
            except OSError:
                pass
        for suffix in (".json", ".consumed"):
            try:
                os.remove(os.path.join(real_verdicts, f"{mailbox_key}{suffix}"))
            except OSError:
                pass


def _resolve_fire_fixture(td, move_id, mailbox_kind="worker", event_count=100):
    """Shared harness for the §2b observation-only tests: a Daemon + one
    worker mailbox, valve.append_telemetry captured, one _resolve_fire call.
    The fire mechanics don't need a transcript — _resolve_fire is pure
    mailbox state + move catalog."""
    session_dir = os.path.join(td, "session")
    os.makedirs(os.path.join(session_dir, "sess1", "subagents"), exist_ok=True)
    d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
    mb = observer.AgentWorker("agshadow1", "sess1", "unused-path") if mailbox_kind == "worker" else None
    captured = []
    orig = valve.append_telemetry
    valve.append_telemetry = captured.append
    try:
        logf = io.StringIO()
        flag = d._resolve_fire(event_count, move_id, {"evidence": "ev", "confidence": 0.9}, logf, mailbox=mb)
        return d, mb, flag, captured, logf
    finally:
        valve.append_telemetry = orig


def test_worker_corrective_fire_shadowed_not_delivered():
    """§2b observation-only mode (2026-07-07): a non-advice fire on a worker
    mailbox runs all bookkeeping but returns None (nothing for a caller to
    deliver) and logs a worker_shadow_fire telemetry record instead."""
    def run(td):
        d, mb, flag, captured, logf = _resolve_fire_fixture(td, "anchor/verify-claim")
        check("worker corrective fire returns None (not delivered)", flag is None)
        shadows = [r for r in captured if r.get("event") == "worker_shadow_fire"]
        check("exactly one worker_shadow_fire telemetry record", len(shadows) == 1, captured)
        if shadows:
            rec = shadows[0]
            check("shadow record carries agent_id", rec.get("agent_id") == "agshadow1", rec)
            check("shadow record carries move_id", rec.get("move_id") == "anchor/verify-claim", rec)
            check("shadow record carries seq", rec.get("seq") == 1, rec)
        check("shadow fire still consumed bookkeeping (fire_count)", mb.fire_count.get("anchor/verify-claim") == 1, mb.fire_count)
        check("shadow fire still consumed a seq", mb.next_seq == 2, mb.next_seq)
        check("shadow fire logged as observation-only", "shadow fire" in logf.getvalue(), logf.getvalue())
    with_temp_dirs(run)


def test_worker_shadow_fire_respects_cooldown():
    """A second immediate fire of the same move hits the cooldown gate BEFORE
    shadow logging — shadow cadence mirrors what live delivery would do."""
    def run(td):
        d, mb, flag, captured, logf = _resolve_fire_fixture(td, "anchor/verify-claim")
        orig = valve.append_telemetry
        valve.append_telemetry = captured.append
        try:
            flag2 = d._resolve_fire(101, "anchor/verify-claim", {"evidence": "ev2", "confidence": 0.9}, io.StringIO(), mailbox=mb)
        finally:
            valve.append_telemetry = orig
        shadows = [r for r in captured if r.get("event") == "worker_shadow_fire"]
        check("second immediate fire suppressed by cooldown, not double-shadowed", flag2 is None and len(shadows) == 1, captured)
    with_temp_dirs(run)


def test_worker_advice_fire_still_delivered():
    """Advice-kind moves (no ack ritual, no file-write ask) keep delivering
    to workers — the §2b whisper-refusal class doesn't apply to them."""
    def run(td):
        d, mb, flag, captured, logf = _resolve_fire_fixture(td, "mechanical/reasoning-primer")
        check("worker advice fire still returns a deliverable flag", flag is not None and flag.get("move_id") == "mechanical/reasoning-primer", flag)
        shadows = [r for r in captured if r.get("event") == "worker_shadow_fire"]
        check("no shadow record for a delivered advice fire", len(shadows) == 0, captured)
    with_temp_dirs(run)


def test_main_session_fire_unaffected_by_observation_only_mode():
    def run(td):
        d, mb, flag, captured, logf = _resolve_fire_fixture(td, "anchor/verify-claim", mailbox_kind="main")
        check("main-session corrective fire still delivers", flag is not None and flag.get("move_id") == "anchor/verify-claim", flag)
        shadows = [r for r in captured if r.get("event") == "worker_shadow_fire"]
        check("no shadow record on the main-session path", len(shadows) == 0, captured)
    with_temp_dirs(run)


def main():
    tests = [
        test_scan_agents_noop_when_flag_absent,
        test_scan_agents_discovers_and_catches_up_when_enabled,
        test_agent_window_closes_and_classifies_independently,
        test_session_mailbox_unaffected_by_agent_activity,
        test_worker_corrective_fire_shadowed_not_delivered,
        test_worker_shadow_fire_respects_cooldown,
        test_worker_advice_fire_still_delivered,
        test_main_session_fire_unaffected_by_observation_only_mode,
        test_scan_agents_discovers_workflow_dir_agent_when_enabled,
        test_scan_agents_discovers_top_level_and_workflow_agents_together,
        test_scan_agents_finds_workflow_run_dir_appearing_after_first_scan,
        test_scan_agents_ignores_non_agent_files_in_workflow_dir,
        test_scan_agents_tolerates_missing_workflows_dir,
        test_posttooluse_hook_agent_event_quiet_without_planted_verdict,
        test_posttooluse_hook_agent_delivery_includes_agent_id_in_ack,
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
