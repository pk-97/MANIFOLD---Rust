"""Tests for the phase-transition shadow tier (DESIGN.md §2d): three
deterministic rules over the phase stream `_handle_window` already builds
from classifier verdicts. SHADOW MODE ONLY — every rule here must log
`phase_fire` telemetry and never write a verdict/mailbox file, never touch
cooldown/escalation state, and never call `_resolve_fire`.

Run: python3 test_phase_transitions.py
"""

import importlib.util
import io
import json
import os
import sys
import tempfile

DAEMON_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, DAEMON_DIR)

spec = importlib.util.spec_from_file_location("observer", os.path.join(DAEMON_DIR, "observer.py"))
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)
import common  # noqa: E402
import valve  # noqa: E402

PASSED = FAILED = 0


def check(label, cond, detail=None):
    global PASSED, FAILED
    if cond:
        PASSED += 1
    else:
        FAILED += 1
        print(f"FAIL: {label}" + (f" — {detail}" if detail is not None else ""))


def with_temp_dirs(fn):
    orig_verdicts = observer.VERDICTS_DIR
    orig_valve_verdicts, orig_valve_telemetry = valve.VERDICTS_DIR, valve.TELEMETRY_PATH
    with tempfile.TemporaryDirectory() as td:
        observer.VERDICTS_DIR = td
        valve.VERDICTS_DIR = td
        valve.TELEMETRY_PATH = os.path.join(td, "telemetry.jsonl")
        try:
            fn(td)
        finally:
            observer.VERDICTS_DIR = orig_verdicts
            valve.VERDICTS_DIR, valve.TELEMETRY_PATH = orig_valve_verdicts, orig_valve_telemetry


def make_daemon():
    return observer.Daemon("test-session", "/dev/null")


def phase_fires(td, rule_id=None):
    path = valve.TELEMETRY_PATH
    if not os.path.exists(path):
        return []
    out = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            rec = json.loads(line)
            if rec.get("event") == "phase_fire" and (rule_id is None or rec.get("move_id") == rule_id):
                out.append(rec)
    return out


def close_window(d, phase, event_count, logf):
    """Directly drives _handle_window with a fake classifier, the same
    pattern test_richer_windows.py uses — no real transcript needed."""

    def fake_classifier(system_prompt, window_text, *a, **kw):
        return {"phase": phase, "flag": None}

    orig = common.call_classifier
    observer.common.call_classifier = fake_classifier
    try:
        d._handle_window({"end_event_count": event_count, "end_ts": float(event_count), "text": "TASK: x"}, logf)
    finally:
        observer.common.call_classifier = orig


# ---- shadow mode: never delivers ----


def test_shadow_mode_never_writes_a_verdict_flag():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d.state.current_task = "why is this crashing"
        d.state.events_since_task = 3
        close_window(d, "implementing", 3, logf)  # would fire rule 1
        with open(d.verdict_path, encoding="utf-8") as f:
            record = json.load(f)
        check("no flag ever written by the phase tier", record.get("flag") is None, record)
        check("phase_fire telemetry logged instead", len(phase_fires(td, "phase/implementing-without-investigating")) == 1)

    with_temp_dirs(run)


def test_shadow_mode_never_touches_cooldown_state():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d.state.current_task = "why is this crashing"
        d.state.events_since_task = 3
        close_window(d, "implementing", 3, logf)
        check("last_fire_event untouched", d.last_fire_event == {})
        check("fire_count untouched", d.fire_count == {})
        check("next_seq untouched (still 1)", d.next_seq == 1)

    with_temp_dirs(run)


# ---- rule 1: implementing-without-investigating ----


def test_rule1_fires_on_diagnosis_task_with_no_investigating():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d.state.current_task = "why does the renderer crash on startup"
        d.state.events_since_task = 2
        close_window(d, "orienting", 2, logf)
        close_window(d, "implementing", 4, logf)
        fires = phase_fires(td, "phase/implementing-without-investigating")
        check("rule 1 fires on the orienting->implementing transition", len(fires) == 1, fires)

    with_temp_dirs(run)


def test_rule1_silent_when_investigating_occurred():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d.state.current_task = "why does the renderer crash on startup"
        d.state.events_since_task = 2
        close_window(d, "orienting", 2, logf)
        close_window(d, "investigating", 4, logf)
        close_window(d, "implementing", 6, logf)
        check("rule 1 silent once investigating happened", phase_fires(td, "phase/implementing-without-investigating") == [])

    with_temp_dirs(run)


def test_rule1_silent_for_non_diagnosis_task():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d.state.current_task = "add a new dark mode toggle to settings"
        d.state.events_since_task = 2
        close_window(d, "orienting", 2, logf)
        close_window(d, "implementing", 4, logf)
        check("rule 1 silent for a non-diagnosis TASK", phase_fires(td, "phase/implementing-without-investigating") == [])

    with_temp_dirs(run)


def test_rule1_only_fires_on_the_transition_not_every_window():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d.state.current_task = "why is this broken"
        d.state.events_since_task = 2
        close_window(d, "orienting", 2, logf)
        close_window(d, "implementing", 4, logf)
        close_window(d, "implementing", 12, logf)  # still implementing, no transition
        check("rule 1 fires exactly once, not per window", len(phase_fires(td, "phase/implementing-without-investigating")) == 1)

    with_temp_dirs(run)


def test_rule1_investigating_before_task_reset_does_not_count():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        # Old task investigated thoroughly...
        d.state.current_task = "why did the old thing break"
        d.state.events_since_task = 2
        d.state.total_tool_event_count = 2
        close_window(d, "investigating", 2, logf)
        # ...then a brand new diagnosis-shaped task starts fresh (task set
        # at event 2 — strictly after the old investigating window, so it
        # must not count towards this task's gate).
        d.state.current_task = "why is this new bug happening"
        d.state.events_since_task = 1
        d.state.total_tool_event_count = 3
        close_window(d, "implementing", 4, logf)
        check(
            "investigating from a prior task does not satisfy the new task's gate",
            len(phase_fires(td, "phase/implementing-without-investigating")) == 1,
        )

    with_temp_dirs(run)


# ---- rule 2: no-verify-before-reporting ----


def test_rule2_fires_when_reporting_with_no_verifying_since_implementing():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        close_window(d, "implementing", 2, logf)
        close_window(d, "reporting", 4, logf)
        check("rule 2 fires on implementing->reporting with no verifying", len(phase_fires(td, "phase/no-verify-before-reporting")) == 1)

    with_temp_dirs(run)


def test_rule2_silent_when_verifying_occurred():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        close_window(d, "implementing", 2, logf)
        close_window(d, "verifying", 4, logf)
        close_window(d, "reporting", 6, logf)
        check("rule 2 silent once verifying happened", phase_fires(td, "phase/no-verify-before-reporting") == [])

    with_temp_dirs(run)


def test_rule2_silent_with_no_prior_implementing():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        close_window(d, "orienting", 2, logf)
        close_window(d, "reporting", 4, logf)
        check("rule 2 silent — nothing to gate against yet", phase_fires(td, "phase/no-verify-before-reporting") == [])

    with_temp_dirs(run)


def test_rule2_only_fires_on_the_transition():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        close_window(d, "implementing", 2, logf)
        close_window(d, "reporting", 4, logf)
        close_window(d, "reporting", 12, logf)  # still reporting, no transition
        check("rule 2 fires exactly once", len(phase_fires(td, "phase/no-verify-before-reporting")) == 1)

    with_temp_dirs(run)


# ---- rule 3: stuck-oscillation ----


def test_rule3_fires_on_enough_flips_in_span():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        for i, phase in enumerate(["implementing", "stuck", "implementing", "stuck"]):
            close_window(d, phase, (i + 1) * 2, logf)
        fires = phase_fires(td, "phase/stuck-oscillation")
        check("rule 3 fires once 3 flips accumulate", len(fires) == 1, fires)

    with_temp_dirs(run)


def test_rule3_silent_below_flip_threshold():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        close_window(d, "implementing", 2, logf)
        close_window(d, "stuck", 4, logf)  # only 1 flip
        check("rule 3 silent under threshold", phase_fires(td, "phase/stuck-oscillation") == [])

    with_temp_dirs(run)


def test_rule3_edge_triggered_not_reflogged_every_window():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        for i, phase in enumerate(["implementing", "stuck", "implementing", "stuck"]):
            close_window(d, phase, (i + 1) * 2, logf)
        close_window(d, "stuck", 10, logf)  # condition still true, no new edge
        check("rule 3 does not re-fire while still oscillating", len(phase_fires(td, "phase/stuck-oscillation")) == 1)

    with_temp_dirs(run)


def test_rule3_refires_on_a_new_rising_edge():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        for i, phase in enumerate(["implementing", "stuck", "implementing", "stuck"]):
            close_window(d, phase, (i + 1) * 2, logf)  # ec 2,4,6,8 -> fires (edge 1)
        # Jump far enough forward (> PHASE_OSCILLATION_SPAN_EVENTS) that the
        # early flips age out of the span entirely, and hold steady — the
        # rule must observe the flip count actually drop before a later
        # oscillation counts as a NEW edge rather than the same one.
        close_window(d, "implementing", 50, logf)  # span_start=10: old flips (ec<=8) excluded, flips=0 -> settles
        close_window(d, "stuck", 52, logf)  # flips=1
        close_window(d, "implementing", 54, logf)  # flips=2
        close_window(d, "stuck", 56, logf)  # flips=3 -> fires again (edge 2)
        check("rule 3 fires again on a fresh rising edge", len(phase_fires(td, "phase/stuck-oscillation")) == 2)

    with_temp_dirs(run)


def test_rule3_ignores_flips_outside_the_span():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        close_window(d, "implementing", 2, logf)
        close_window(d, "stuck", 4, logf)
        # Large event-count jump pushes the early flip outside the span.
        close_window(d, "implementing", 200, logf)
        close_window(d, "stuck", 202, logf)
        check("stale flips outside the span don't count towards the threshold", phase_fires(td, "phase/stuck-oscillation") == [])

    with_temp_dirs(run)


# ---- validate_move_id / phase/ prefix (shared plumbing verification) ----


def test_phase_move_id_never_valid_from_classifier():
    moves = common.parse_moves(common.read(observer.MOVES_PATH))
    check(
        "no real phase/ id in the moves.md catalog would validate anyway",
        common.validate_move_id("phase/implementing-without-investigating", moves) is None,
    )


# ---- firestate persistence across a revive ----


def test_phase_history_persists_across_revive():
    def run(td):
        d1 = make_daemon()
        logf = io.StringIO()
        close_window(d1, "implementing", 2, logf)
        close_window(d1, "stuck", 4, logf)
        check("phase_history recorded before revive", len(d1.phase_history) == 2)

        # Simulate an idle-exit + revive: a fresh Daemon instance for the
        # same session_id must pick up the persisted phase stream.
        d2 = make_daemon()
        check("phase_history survives a revive via firestate", d2.phase_history == d1.phase_history, (d2.phase_history, d1.phase_history))

    with_temp_dirs(run)


def test_worker_phase_history_stays_in_memory_only():
    def run(td):
        worker = observer.AgentWorker("agent1", "test-session", "/dev/null")
        check("worker starts with empty phase_history", worker.phase_history == [])
        check("worker has no firestate path", not hasattr(worker, "fire_state_path"))

    with_temp_dirs(run)


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
    print(f"\n{PASSED} passed, {FAILED} failed")
    sys.exit(1 if FAILED else 0)
