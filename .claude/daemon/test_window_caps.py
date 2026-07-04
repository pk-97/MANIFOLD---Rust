#!/usr/bin/env python3
"""
Standalone test for the window-size discipline (WINDOW_VERSION 4): TASK and
RECENT hard caps, and harness-injected user texts never becoming TASK.
Regression guard for the 2026-07-04 orchestrator incident (session cadd7aad):
a <task-notification> carrying a worker's full report became current_task,
windows grew to hundreds of KB, and every classifier call timed out.

Run: python3 .claude/daemon/test_window_caps.py
"""
import sys
from pathlib import Path

DAEMON_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(DAEMON_DIR))
import common  # noqa: E402

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


def close_via_tool_events(state):
    """Drive enough tool events through the state to close a window."""
    closed = None
    for i in range(common.CADENCE_EVENTS):
        state.feed_assistant_content([{"type": "tool_use", "id": f"t{i}", "name": "Read", "input": {"file_path": f"f{i}.rs"}}])
        closed, _ = state.feed_user_content(
            [{"type": "tool_result", "tool_use_id": f"t{i}", "is_error": False, "content": ""}], ts=1000.0 + i
        )
    return closed


def test_task_is_capped():
    state = common.WindowState()
    state.feed_user_content("fix the widget " * 500, ts=1.0)  # ~7.5KB instruction
    check("oversized task capped", len(state.current_task) <= common.TASK_MAX_CHARS, len(state.current_task))
    check("capped task keeps its head", state.current_task.startswith("fix the widget"))


def test_recent_is_capped():
    state = common.WindowState()
    state.feed_user_content("do the thing", ts=1.0)
    state.feed_assistant_content([{"type": "text", "text": "report line\n" * 2000}])  # ~24KB reply
    check(
        "oversized assistant text capped in RECENT",
        all(len(t) <= common.RECENT_MAX_CHARS for t in state.recent_texts),
        [len(t) for t in state.recent_texts],
    )
    check("capped recent keeps newlines", "\n" in state.recent_texts[-1])


def test_window_text_is_bounded_under_orchestrator_load():
    """The incident shape: giant task-notifications + long worker reports.
    Whatever lands, a closed window must stay a few KB."""
    state = common.WindowState()
    state.feed_user_content("orchestrate the two lanes", ts=1.0)
    state.feed_user_content("<task-notification>\n" + "worker report " * 20000, ts=2.0)
    state.feed_assistant_content([{"type": "text", "text": "synthesis " * 10000}])
    closed = close_via_tool_events(state)
    check("window closed", closed is not None)
    check("window text bounded", closed and len(closed["text"]) < 10_000, closed and len(closed["text"]))


def test_harness_texts_never_become_task():
    state = common.WindowState()
    state.feed_user_content("fix the failing parity test", ts=1.0)
    for prefix in common.HARNESS_TEXT_PREFIXES:
        state.feed_user_content(f"{prefix}some harness payload that is plenty long</x>", ts=2.0)
    check("task survives harness texts", state.current_task == "fix the failing parity test", state.current_task)


def test_harness_texts_still_count_as_human_text_seen():
    """Callers (replay gate math) still see the raw text; only TASK is guarded."""
    state = common.WindowState()
    _, human = state.feed_user_content("<system-reminder>hook says something</system-reminder>", ts=1.0)
    check("harness text still returned to caller", len(human) == 1)


def test_real_user_task_still_sets_and_resets():
    state = common.WindowState()
    state.feed_user_content("first task", ts=1.0)
    state.feed_assistant_content([{"type": "text", "text": "a reply comfortably past the addressed threshold, yes"}])
    state.feed_user_content("second task after a correction", ts=2.0)
    check("new real text replaces task", state.current_task == "second task after a correction")
    check("task_addressed reset", state.task_addressed is False)
    check("events_since_task reset", state.events_since_task == 0)


def main():
    tests = [
        test_task_is_capped,
        test_recent_is_capped,
        test_window_text_is_bounded_under_orchestrator_load,
        test_harness_texts_never_become_task,
        test_harness_texts_still_count_as_human_text_seen,
        test_real_user_task_still_sets_and_resets,
    ]
    for t in tests:
        t()
    for name in PASS:
        print(f"PASS {name}")
    for name, detail in FAIL:
        print(f"FAIL {name} {detail}")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
