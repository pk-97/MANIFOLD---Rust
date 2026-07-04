#!/usr/bin/env python3
"""
Standalone test for DESIGN.md §2c stopgap detection: common.py's
STOPGAP_MARKERS / detect_stopgap_markers (tier 1 + the tier 3 ledger
annotation) and observer.py's deterministic mechanical/confessed-stopgap
fire path (_check_stopgap). Never spawns a real classifier call — the
mechanical fire path bypasses Haiku entirely, by design.

Run: python3 .claude/daemon/test_stopgap_detection.py
"""
import importlib.util
import io
import json
import os
import sys
import tempfile
from pathlib import Path

DAEMON_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(DAEMON_DIR))
import common  # noqa: E402

spec = importlib.util.spec_from_file_location("observer", DAEMON_DIR / "observer.py")
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)
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


# ---- common.detect_stopgap_markers ----


def test_edit_adds_hack_word():
    hits = common.detect_stopgap_markers(
        "Edit", {"file_path": "foo.rs", "old_string": "let x = 1;", "new_string": "let x = 1; // HACK: skip validation"}
    )
    check("hack-word detected in added text", "hack-word" in hits, hits)


def test_edit_removing_hack_never_fires():
    hits = common.detect_stopgap_markers(
        "Edit", {"file_path": "foo.rs", "old_string": "// HACK: skip validation\nlet x = 1;", "new_string": "let x = 1;"}
    )
    check("removing a hack does not fire", hits == [], hits)


def test_edit_marker_present_in_both_old_and_new_does_not_fire():
    hits = common.detect_stopgap_markers(
        "Edit",
        {
            "file_path": "foo.rs",
            "old_string": "// workaround for driver bug\nlet x = 1;",
            "new_string": "// workaround for driver bug\nlet x = 2;",
        },
    )
    check("unchanged marker (present in both) does not re-fire", hits == [], hits)


def test_write_whole_body_counts_as_added():
    hits = common.detect_stopgap_markers(
        "Write", {"file_path": "foo.rs", "content": "fn f() {\n    // for now, just return zero\n}"}
    )
    check("Write body scanned whole, for-now detected", "for-now" in hits, hits)


def test_multiedit_aggregates_across_edits():
    hits = common.detect_stopgap_markers(
        "MultiEdit",
        {
            "file_path": "foo.rs",
            "edits": [
                {"old_string": "a", "new_string": "a // TODO: revisit this later"},
                {"old_string": "b", "new_string": "#[allow(dead_code)]\nfn b() {}"},
            ],
        },
    )
    check("MultiEdit aggregates deferral", "deferral" in hits, hits)
    check("MultiEdit aggregates lint-suppression", "lint-suppression" in hits, hits)


def test_deferral_requires_todo_near_deferral_word():
    hits = common.detect_stopgap_markers(
        "Edit", {"file_path": "foo.rs", "old_string": "x", "new_string": "// TODO: rename this variable to something clearer"}
    )
    check("bare TODO with no deferral word does not fire deferral", "deferral" not in hits, hits)


def test_fixme_always_fires_deferral():
    hits = common.detect_stopgap_markers("Edit", {"file_path": "foo.rs", "old_string": "x", "new_string": "// FIXME"})
    check("bare FIXME fires deferral", "deferral" in hits, hits)


def test_markdown_files_excluded():
    hits = common.detect_stopgap_markers(
        "Write", {"file_path": "docs/NOTES.md", "content": "This is a HACK we should fix, for now."}
    )
    check("*.md excluded entirely", hits == [], hits)


def test_claude_internals_excluded():
    hits = common.detect_stopgap_markers(
        "Edit", {"file_path": ".claude/daemon/observer.py", "old_string": "x", "new_string": "x  # HACK for now"}
    )
    check(".claude/ paths excluded entirely", hits == [], hits)


def test_race_sleep_excluded_in_test_paths():
    for path in ("crates/foo/tests/bar.rs", "crates/foo/src/test_bar.rs", "crates/foo/src/bar_test.rs"):
        hits = common.detect_stopgap_markers(
            "Edit", {"file_path": path, "old_string": "x", "new_string": "thread::sleep(Duration::from_millis(50));"}
        )
        check(f"race-sleep excluded in test path {path}", "race-sleep" not in hits, (path, hits))


def test_race_sleep_fires_outside_test_paths():
    hits = common.detect_stopgap_markers(
        "Edit", {"file_path": "crates/foo/src/sync.rs", "old_string": "x", "new_string": "thread::sleep(Duration::from_millis(50));"}
    )
    check("race-sleep fires in non-test path", "race-sleep" in hits, hits)


def test_non_edit_tool_never_fires():
    hits = common.detect_stopgap_markers("Bash", {"command": "echo HACK for now workaround FIXME"})
    check("non-Edit/Write/MultiEdit tool never scanned", hits == [], hits)


def test_malformed_input_never_raises():
    hits = common.detect_stopgap_markers("Edit", "not-a-dict")
    check("malformed input returns empty, no raise", hits == [], hits)
    hits2 = common.detect_stopgap_markers("MultiEdit", {"file_path": "foo.rs", "edits": "not-a-list"})
    check("malformed edits list returns empty, no raise", hits2 == [], hits2)


# ---- tier 3: ledger annotation ----


def tool_result(state, tool_use_id, name, input_, is_error=False):
    state.feed_assistant_content([{"type": "tool_use", "id": tool_use_id, "name": name, "input": input_}])
    content = [{"type": "tool_result", "tool_use_id": tool_use_id, "is_error": is_error, "content": ""}]
    return state.feed_user_content(content, ts=1000.0)


def test_ledger_annotates_stopgap_hit():
    state = common.WindowState()
    tool_result(
        state, "a", "Edit",
        {"file_path": "foo.rs", "old_string": "x", "new_string": "x // HACK for now"},
    )
    check("ledger line carries adds: annotation", "(adds:" in state.ledger_buffer[0], state.ledger_buffer[0])
    check("hack-word category named", "hack-word" in state.ledger_buffer[0], state.ledger_buffer[0])


def test_ledger_no_annotation_when_clean():
    state = common.WindowState()
    tool_result(state, "a", "Edit", {"file_path": "foo.rs", "old_string": "x", "new_string": "y"})
    check("no adds: annotation on clean edit", "(adds:" not in state.ledger_buffer[0], state.ledger_buffer[0])


def test_window_version_is_3():
    check("WINDOW_VERSION bumped to 3", common.WINDOW_VERSION == 3)


# ---- observer.py tier 1: deterministic mechanical fire ----


def make_daemon():
    return observer.Daemon("test-session", "/dev/null")


def test_check_stopgap_fires_flag_for_main_session():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_stopgap("Edit", {"file_path": "foo.rs", "old_string": "x", "new_string": "x // HACK for now"}, event_count=1, logf=logf)
        with open(d.verdict_path, encoding="utf-8") as f:
            record = json.load(f)
        check("flag written", record.get("flag") is not None, record)
        check("move_id is mechanical/confessed-stopgap", record["flag"]["move_id"] == "mechanical/confessed-stopgap", record)
        check("window_version on record", record.get("window_version") == 3, record)
    with_temp_dirs(run)


def test_check_stopgap_no_hit_writes_nothing():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_stopgap("Edit", {"file_path": "foo.rs", "old_string": "x", "new_string": "y"}, event_count=1, logf=logf)
        check("no verdict file written when clean", not os.path.exists(d.verdict_path))
    with_temp_dirs(run)


def test_check_stopgap_respects_cooldown():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        edit = {"file_path": "foo.rs", "old_string": "x", "new_string": "x // HACK for now"}
        d._check_stopgap("Edit", edit, event_count=1, logf=logf)
        with open(d.verdict_path, encoding="utf-8") as f:
            first = json.load(f)
        # Consume the first flag so a second fire wouldn't be suppressed by
        # the "one live flag at a time" rule instead of by cooldown.
        with open(d.consumed_path, "w", encoding="utf-8") as f:
            f.write(str(first["flag"]["seq"]))
        d._check_stopgap("Edit", edit, event_count=5, logf=logf)  # well within standard cooldown (20)
        with open(d.verdict_path, encoding="utf-8") as f:
            second = json.load(f)
        check("cooldown suppresses re-fire within 20 events", second["flag"]["seq"] == first["flag"]["seq"], (first, second))

        d._check_stopgap("Edit", edit, event_count=25, logf=logf)  # past cooldown
        with open(d.verdict_path, encoding="utf-8") as f:
            third = json.load(f)
        check("fires again once cooldown elapses", third["flag"]["seq"] != first["flag"]["seq"], (first, third))
    with_temp_dirs(run)


def test_stopgap_never_fires_during_catchup():
    def run(td):
        session_dir = os.path.join(td, "session")
        os.makedirs(session_dir)
        transcript = os.path.join(session_dir, "sess1.jsonl")
        with open(transcript, "w", encoding="utf-8") as f:
            f.write(json.dumps({"type": "user", "message": {"role": "user", "content": "fix the widget please"}, "timestamp": "2026-07-04T00:00:00Z"}) + "\n")
            f.write(json.dumps({
                "type": "assistant",
                "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [
                    {"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": "foo.rs", "old_string": "x", "new_string": "x // HACK for now"}},
                ]},
                "timestamp": "2026-07-04T00:00:01Z",
            }) + "\n")
            f.write(json.dumps({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "is_error": False, "content": ""},
            ]}, "timestamp": "2026-07-04T00:00:02Z"}) + "\n")
        d = observer.Daemon("sess1", transcript)
        logf = io.StringIO()
        d._catchup(logf)  # classify=False internally
        check("no verdict file after catchup-only replay", not os.path.exists(d.verdict_path))
    with_temp_dirs(run)


def test_worker_stopgap_routes_to_agent_mailbox_when_enabled():
    def run(td):
        with open(observer.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session_dir = os.path.join(td, "session")
        os.makedirs(os.path.join(session_dir, "subagents"))
        agent_path = os.path.join(session_dir, "subagents", "agent-abc123.jsonl")
        with open(agent_path, "w", encoding="utf-8") as f:
            f.write(json.dumps({"type": "user", "message": {"role": "user", "content": "refactor this please"}, "timestamp": "2026-07-04T00:00:00Z"}) + "\n")
            f.write(json.dumps({
                "type": "assistant",
                "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [
                    {"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": "foo.rs", "old_string": "x", "new_string": "x // HACK for now"}},
                ]},
                "timestamp": "2026-07-04T00:00:01Z",
            }) + "\n")
            f.write(json.dumps({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "is_error": False, "content": ""},
            ]}, "timestamp": "2026-07-04T00:00:02Z"}) + "\n")
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
        d.session_dir = session_dir
        logf = io.StringIO()
        d._scan_agents(logf)  # discovers + catchup (classify=False) — should NOT fire yet
        worker = d.agents.get("abc123")
        check("agent discovered", worker is not None)
        check("no fire on agent catchup", not os.path.exists(worker.verdict_path) if worker else False)

        # Append another tool event so the live-tail (classify=True) path runs.
        with open(agent_path, "a", encoding="utf-8") as f:
            f.write(json.dumps({
                "type": "assistant",
                "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [
                    {"type": "tool_use", "id": "t2", "name": "Edit", "input": {"file_path": "bar.rs", "old_string": "x", "new_string": "x // HACK for now"}},
                ]},
                "timestamp": "2026-07-04T00:00:03Z",
            }) + "\n")
            f.write(json.dumps({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t2", "is_error": False, "content": ""},
            ]}, "timestamp": "2026-07-04T00:00:04Z"}) + "\n")
        d._scan_agents(logf)

        check("worker mailbox got the flag", os.path.exists(worker.verdict_path))
        check("session mailbox untouched by worker stopgap fire", not os.path.exists(d.verdict_path))
        with open(worker.verdict_path, encoding="utf-8") as f:
            record = json.load(f)
        check("worker flag is mechanical/confessed-stopgap", record["flag"]["move_id"] == "mechanical/confessed-stopgap", record)
    with_temp_dirs(run)


def main():
    tests = [
        test_edit_adds_hack_word,
        test_edit_removing_hack_never_fires,
        test_edit_marker_present_in_both_old_and_new_does_not_fire,
        test_write_whole_body_counts_as_added,
        test_multiedit_aggregates_across_edits,
        test_deferral_requires_todo_near_deferral_word,
        test_fixme_always_fires_deferral,
        test_markdown_files_excluded,
        test_claude_internals_excluded,
        test_race_sleep_excluded_in_test_paths,
        test_race_sleep_fires_outside_test_paths,
        test_non_edit_tool_never_fires,
        test_malformed_input_never_raises,
        test_ledger_annotates_stopgap_hit,
        test_ledger_no_annotation_when_clean,
        test_window_version_is_3,
        test_check_stopgap_fires_flag_for_main_session,
        test_check_stopgap_no_hit_writes_nothing,
        test_check_stopgap_respects_cooldown,
        test_stopgap_never_fires_during_catchup,
        test_worker_stopgap_routes_to_agent_mailbox_when_enabled,
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
