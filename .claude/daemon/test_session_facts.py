"""Tests for the session-fact store (DESIGN.md §2f): last-verification-per-
class, the latest TASK-set context switch, and edited-path last-read facts,
all rendered as a SESSION FACTS: block appended to the window text so they
outlive the ~8-event ledger horizon. Regex-tier extraction only, same as
STOPGAP_MARKERS — no classifier calls.

Run: python3 test_session_facts.py
"""

import os
import sys

DAEMON_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, DAEMON_DIR)
import common  # noqa: E402

PASSED = FAILED = 0


def check(label, cond, detail=None):
    global PASSED, FAILED
    if cond:
        PASSED += 1
    else:
        FAILED += 1
        print(f"FAIL: {label}" + (f" — {detail}" if detail is not None else ""))


def tool_result(state, tool_use_id, name, input_, is_error=False, ts=1000.0):
    state.feed_assistant_content([{"type": "tool_use", "id": tool_use_id, "name": name, "input": input_}])
    content = [{"type": "tool_result", "tool_use_id": tool_use_id, "is_error": is_error, "content": ""}]
    return state.feed_user_content(content, ts=ts)


# ---- detect_verification_class ----


def test_detect_test_run():
    check("cargo test", common.detect_verification_class("Bash", {"command": "cargo test -p foo --lib"}) == "test-run")
    check("pytest", common.detect_verification_class("Bash", {"command": "pytest tests/"}) == "test-run")
    check("cargo bench", common.detect_verification_class("Bash", {"command": "cargo bench"}) == "test-run")


def test_detect_lint():
    check("cargo clippy", common.detect_verification_class("Bash", {"command": "cargo clippy --workspace -- -D warnings"}) == "lint")


def test_detect_script_run():
    check("cargo run", common.detect_verification_class("Bash", {"command": "cargo run --bin foo"}) == "script-run")
    check("python script", common.detect_verification_class("Bash", {"command": "python3 scratch/check.py"}) == "script-run")


def test_detect_render_read():
    check("read png", common.detect_verification_class("Read", {"file_path": "out/frame.png"}) == "render-read")
    check("read non-png", common.detect_verification_class("Read", {"file_path": "src/lib.rs"}) is None)


def test_detect_none_for_unrelated():
    check("Edit is never a verification class", common.detect_verification_class("Edit", {"file_path": "x.rs"}) is None)
    check("plain ls is not a verification class", common.detect_verification_class("Bash", {"command": "ls -la"}) is None)
    check("non-dict input never raises", common.detect_verification_class("Bash", None) is None)


# ---- last_verification durability past the ledger horizon ----


def test_last_verification_survives_past_ledger_horizon():
    state = common.WindowState()
    state.feed_user_content("investigate the failing widget test please", ts=1.0)
    tool_result(state, "t0", "Bash", {"command": "cargo test -p manifold-renderer --lib"})
    # push the verifying event well outside the ~8-event ledger by closing
    # several more windows with unrelated tool events
    for w in range(3):
        for i in range(8):
            tool_result(state, f"w{w}i{i}", "Bash", {"command": "ls"})
    closed = state._close_window(2.0)
    check(
        "last test-run fact still present many events later",
        "last test-run: event 1," in closed["text"] and "cargo test -p manifold-renderer" in closed["text"],
        closed["text"],
    )


def test_last_verification_reports_events_ago():
    state = common.WindowState()
    tool_result(state, "t0", "Bash", {"command": "cargo clippy --workspace"})
    for i in range(4):
        tool_result(state, f"t{i}", "Bash", {"command": "ls"})
    closed = state._close_window(2.0)
    check("lint fact reports staleness", "last lint: event 1, 4 events ago" in closed["text"], closed["text"])


def test_all_four_classes_tracked_independently():
    state = common.WindowState()
    tool_result(state, "a", "Bash", {"command": "cargo test -p foo"})
    tool_result(state, "b", "Bash", {"command": "cargo clippy"})
    tool_result(state, "c", "Bash", {"command": "cargo run --bin foo"})
    tool_result(state, "d", "Read", {"file_path": "out/render.png"})
    closed = state._close_window(2.0)
    for cls in ("test-run", "lint", "script-run", "render-read"):
        check(f"{cls} present in facts block", f"last {cls}:" in closed["text"], closed["text"])


# ---- context_switches ----


def test_context_switch_recorded_on_task_set():
    state = common.WindowState()
    state.feed_user_content("do the first thing please, make it good", ts=1.0)
    check("first switch recorded at event 0", state.context_switches == [(0, "do the first thing please, make it good")])
    tool_result(state, "a", "Bash", {"command": "ls"})
    state.feed_user_content("actually switch to the second thing entirely", ts=2.0)
    check("second switch recorded with correct event count", state.context_switches[-1][0] == 1)


def test_latest_context_switch_rendered_in_facts_block():
    state = common.WindowState()
    state.feed_user_content("first task statement goes here please", ts=1.0)
    tool_result(state, "a", "Bash", {"command": "ls"})
    state.feed_user_content("second and different task statement now", ts=2.0)
    closed = state._close_window(3.0)
    check("only the latest switch renders", "TASK set by user at event 1" in closed["text"], closed["text"])
    check("stale switch not rendered", "TASK set by user at event 0" not in closed["text"], closed["text"])


def test_harness_text_never_becomes_context_switch():
    state = common.WindowState()
    state.feed_user_content("<system-reminder>some hook text that is long enough</system-reminder>", ts=1.0)
    check("harness text does not create a context switch", state.context_switches == [])


# ---- edited-path last-read facts ----


def test_edit_after_read_same_window_renders_fact():
    state = common.WindowState()
    tool_result(state, "r", "Read", {"file_path": "src/lib.rs"})
    tool_result(state, "e", "Edit", {"file_path": "src/lib.rs"})
    closed = state._close_window(2.0)
    check("edit-after-read fact rendered", "file src/lib.rs last read event 1" in closed["text"], closed["text"])


def test_edit_without_prior_read_renders_no_fact():
    state = common.WindowState()
    tool_result(state, "e", "Edit", {"file_path": "src/never_read.rs"})
    closed = state._close_window(2.0)
    check("no fact for a never-read path", "last read event" not in closed["text"], closed["text"])


def test_edit_with_read_fact_scoped_to_the_closing_window():
    state = common.WindowState()
    tool_result(state, "r", "Read", {"file_path": "src/lib.rs"})
    tool_result(state, "e", "Edit", {"file_path": "src/lib.rs"})
    state._close_window(2.0)  # first window: fact rendered and then reset
    for i in range(8):
        tool_result(state, f"t{i}", "Bash", {"command": "ls"})
    closed = state._close_window(3.0)
    check("edit-read fact does not leak into a later, unrelated window", "last read event" not in closed["text"], closed["text"])


# ---- task_addressed unchanged ----


def test_task_addressed_bit_unaffected():
    state = common.WindowState()
    state.feed_user_content("investigate the stale widget please now", ts=1.0)
    state.feed_assistant_content(
        [{"type": "text", "text": "Looked into it — the widget was stale because of a caching bug, fixed now."}]
    )
    check("task_addressed still sets as before", state.task_addressed is True)


# ---- WINDOW_VERSION ----


def test_window_version_bumped():
    check("WINDOW_VERSION is at least 5", common.WINDOW_VERSION >= 5, common.WINDOW_VERSION)


# ---- catchup-replay durability (no firestate needed — see common.py comment) ----


def test_facts_rebuild_identically_on_replay():
    """Simulates two independent WindowState instances processing the same
    event stream — the exact shape of a daemon restart's catchup replay
    (which always starts from transcript offset 0). If facts are a pure
    function of the event stream, a second pass reconstructs them
    byte-identical to the first, with no persistence required."""

    def replay_into(state):
        state.feed_user_content("investigate the flaky test please", ts=1.0)
        tool_result(state, "a", "Bash", {"command": "cargo test -p foo"})
        tool_result(state, "b", "Read", {"file_path": "src/foo.rs"})
        tool_result(state, "c", "Edit", {"file_path": "src/foo.rs"})
        return state._close_window(2.0)

    first = replay_into(common.WindowState())
    second = replay_into(common.WindowState())
    check("catchup replay reproduces identical window text", first["text"] == second["text"], (first["text"], second["text"]))


# ---- validate_move_id / catalog exclusion for the phase/ family (shared prefix work) ----


def test_phase_prefix_excluded_like_mechanical():
    moves = {"phase/implementing-without-investigating": {"signature": "x", "cooldown": "standard", "kind": "alert", "payload": "y"}}
    check("phase/ ids rejected by validate_move_id", common.validate_move_id("phase/implementing-without-investigating", moves) is None)
    check("phase/ ids excluded from the classifier catalog", "phase/implementing-without-investigating" not in common.build_signature_catalog(moves))


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
    print(f"\n{PASSED} passed, {FAILED} failed")
    sys.exit(1 if FAILED else 0)
