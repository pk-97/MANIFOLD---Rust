#!/usr/bin/env python3
"""
Standalone test for common.py's §4c-1 "richer windows" additions: repeat-
target ledger annotations, consecutive-failure streaks, events-since-TASK,
and window_version on the verdict record. Exercises WindowState directly
with synthetic content blocks — no transcript files, no classifier calls.

Run: python3 .claude/daemon/test_richer_windows.py
"""
import importlib.util
import io
import json
import sys
import tempfile
from pathlib import Path

DAEMON_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(DAEMON_DIR))
import common  # noqa: E402

spec = importlib.util.spec_from_file_location("observer", DAEMON_DIR / "observer.py")
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


def tool_result(state, tool_use_id, name, input_, is_error=False):
    state.feed_assistant_content([{"type": "tool_use", "id": tool_use_id, "name": name, "input": input_}])
    content = [{"type": "tool_result", "tool_use_id": tool_use_id, "is_error": is_error, "content": ""}]
    return state.feed_user_content(content, ts=1000.0)


def test_repeat_target_annotation():
    state = common.WindowState()
    for i in range(4):
        closed, _ = tool_result(state, f"t{i}", "Read", {"file_path": "foo.rs"})
    check("1st touch unannotated", "(1st touch this session)" not in state.ledger_buffer[0])
    check("2nd touch annotated", "(2nd touch this session)" in state.ledger_buffer[1])
    check("4th touch annotated", "(4th touch this session)" in state.ledger_buffer[3])


def test_repeat_target_scoped_to_tool_and_target():
    state = common.WindowState()
    tool_result(state, "a", "Read", {"file_path": "foo.rs"})
    tool_result(state, "b", "Read", {"file_path": "bar.rs"})  # different target, no repeat
    tool_result(state, "c", "Edit", {"file_path": "foo.rs"})  # different tool, no repeat
    check("different target not counted as repeat", "touch this session" not in state.ledger_buffer[1])
    check("different tool not counted as repeat", "touch this session" not in state.ledger_buffer[2])


def test_repeat_target_ignores_non_read_edit_tools():
    state = common.WindowState()
    for i in range(3):
        tool_result(state, f"t{i}", "Bash", {"command": "cargo test"})
    check("Bash repeats never annotated", all("touch this session" not in l for l in state.ledger_buffer))


def test_consecutive_failure_streak():
    state = common.WindowState()
    tool_result(state, "a", "Bash", {"command": "cargo build"}, is_error=True)
    tool_result(state, "b", "Bash", {"command": "cargo build"}, is_error=True)
    tool_result(state, "c", "Bash", {"command": "cargo build"}, is_error=True)
    check("1st failure unannotated", "consecutive failure" not in state.ledger_buffer[0])
    check("2nd consecutive failure annotated", "(2nd consecutive failure)" in state.ledger_buffer[1])
    check("3rd consecutive failure annotated", "(3rd consecutive failure)" in state.ledger_buffer[2])


def test_failure_streak_resets_on_success():
    state = common.WindowState()
    tool_result(state, "a", "Bash", {"command": "x"}, is_error=True)
    tool_result(state, "b", "Bash", {"command": "x"}, is_error=True)
    tool_result(state, "c", "Bash", {"command": "x"}, is_error=False)
    tool_result(state, "d", "Bash", {"command": "x"}, is_error=True)
    check("streak reset after a success", "consecutive failure" not in state.ledger_buffer[3])
    check("state counter reset", state.consecutive_failures == 1)


def test_events_since_task_counts_and_resets():
    state = common.WindowState()
    state.feed_user_content("do the thing, make it work please", ts=1.0)
    check("fresh task starts at 0", state.events_since_task == 0)
    for i in range(3):
        tool_result(state, f"t{i}", "Bash", {"command": "x"})
    check("counts tool events since task", state.events_since_task == 3)
    state.feed_user_content("a brand new different task statement here", ts=2.0)
    check("resets on a new task", state.events_since_task == 0)


def test_events_since_task_appears_in_window_text():
    state = common.WindowState()
    state.feed_user_content("investigate the failing widget test please", ts=1.0)
    for i in range(5):
        tool_result(state, f"t{i}", "Bash", {"command": "x"})
    closed, _ = tool_result(state, "last", "Bash", {"command": "x"})
    # cadence hasn't fired (only 6 events), force a close to inspect the text
    window = state._close_window(2.0)
    check("TASK line carries events-since-set", "6 tool events since set" in window["text"], window["text"])


def test_window_version_on_verdict_record():
    check("WINDOW_VERSION is 2", common.WINDOW_VERSION == 2)
    with tempfile.TemporaryDirectory() as td:
        orig_verdicts = observer.VERDICTS_DIR
        observer.VERDICTS_DIR = td
        try:
            d = observer.Daemon("test-session", "/dev/null")

            def fake_classifier(system_prompt, window_text, *a, **kw):
                return {"phase": "verifying", "flag": None}

            orig = common.call_classifier
            observer.common.call_classifier = fake_classifier
            try:
                logf = io.StringIO()
                d._handle_window({"end_event_count": 1, "end_ts": 1.0, "text": "TASK: x"}, logf)
                with open(d.verdict_path, encoding="utf-8") as f:
                    record = json.load(f)
                check("verdict record carries window_version", record.get("window_version") == 2, record)
            finally:
                observer.common.call_classifier = orig
        finally:
            observer.VERDICTS_DIR = orig_verdicts


def main():
    tests = [
        test_repeat_target_annotation,
        test_repeat_target_scoped_to_tool_and_target,
        test_repeat_target_ignores_non_read_edit_tools,
        test_consecutive_failure_streak,
        test_failure_streak_resets_on_success,
        test_events_since_task_counts_and_resets,
        test_events_since_task_appears_in_window_text,
        test_window_version_on_verdict_record,
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
