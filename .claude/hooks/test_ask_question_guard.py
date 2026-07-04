#!/usr/bin/env python3
"""
Standalone test runner for the AskUserQuestion shortcut-fork guard
(DESIGN.md §2c-ask): common.py's detect_shortcut_fork and
ask-question-guard.py's PreToolUse main(). Invokes both directly with
synthetic input — never spawns a real hook subprocess against a live
session (per DESIGN.md: "test hooks by invoking them directly with
synthetic stdin, not by observing your own session").

Run: python3 .claude/hooks/test_ask_question_guard.py
"""
import importlib.util
import io
import json
import sys
import tempfile
from contextlib import redirect_stdout
from pathlib import Path

HOOKS_DIR = Path(__file__).resolve().parent
DAEMON_DIR = HOOKS_DIR.parent / "daemon"
sys.path.insert(0, str(DAEMON_DIR))
import common  # noqa: E402

HOOK_PATH = HOOKS_DIR / "ask-question-guard.py"
spec = importlib.util.spec_from_file_location("ask_question_guard", HOOK_PATH)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

import valve  # noqa: E402 — same cached module hook.main()'s inner `import valve` sees

# Tests use fake session ids / transcript paths that don't correspond to a
# real session — with the real ensure_observer, that would spawn a genuine
# observer.py subprocess against a nonexistent transcript on every test run.
# Neutralize it for this file only; every other behavior under test
# (detection, deny reason, bounce-once) is unaffected since ensure_observer
# is a fire-and-forget side effect the hook never branches on.
valve.ensure_observer = lambda *a, **kw: None

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


def with_bounce_dir(fn):
    orig = hook.BOUNCE_DIR
    with tempfile.TemporaryDirectory() as td:
        hook.BOUNCE_DIR = td
        try:
            fn(td)
        finally:
            hook.BOUNCE_DIR = orig


def run_hook(payload):
    """Feed `payload` as stdin JSON to hook.main(), return stdout (str,
    empty if the hook emitted nothing)."""
    buf = io.StringIO()
    orig_stdin = sys.stdin
    sys.stdin = io.StringIO(json.dumps(payload))
    try:
        with redirect_stdout(buf):
            hook.main()
    finally:
        sys.stdin = orig_stdin
    return buf.getvalue().strip()


INCIDENT_QUESTIONS = [
    {
        "question": "Approximate transform now, or build the real primitive?",
        "header": "Transform",
        "options": [
            {
                "label": "Approximate, no new primitive (Recommended)",
                "description": "Cheap for now, ships this session without a new node.",
            },
            {
                "label": "Real transform primitive",
                "description": "The proper fundamental fix — a new centered-scale/rotation primitive.",
            },
        ],
        "multiSelect": False,
    }
]

DESTRUCTIVE_CONFIRM_QUESTIONS = [
    {
        "question": "Force-push will overwrite the remote branch. Proceed?",
        "header": "Confirm",
        "options": [
            {"label": "Yes, force-push", "description": "Overwrite origin with local history."},
            {"label": "No, cancel", "description": "Leave the remote branch untouched."},
        ],
        "multiSelect": False,
    }
]

TASTE_CALL_QUESTIONS = [
    {
        "question": "Which color for the active-clip highlight?",
        "header": "Color",
        "options": [
            {"label": "Warm amber (Recommended)", "description": "Matches the existing accent palette."},
            {"label": "Cool cyan", "description": "Higher contrast against dark panels."},
        ],
        "multiSelect": False,
    }
]

MULTISELECT_FEATURE_QUESTIONS = [
    {
        "question": "Which stem lanes should ingest track this session?",
        "header": "Lanes",
        "options": [
            {"label": "Vocals", "description": "Route the vocal stem."},
            {"label": "Drums", "description": "Route the drum stem."},
            {"label": "Bass", "description": "Route the bass stem."},
        ],
        "multiSelect": True,
    }
]

RECOMMENDED_IS_ROOT_FIX_QUESTIONS = [
    {
        "question": "Fix the sync bug now?",
        "header": "Sync fix",
        "options": [
            {"label": "Full root-cause fix (Recommended)", "description": "Fix the fundamental clock drift."},
            {"label": "Quick stopgap for now", "description": "Patch the symptom, defer the real fix."},
        ],
        "multiSelect": False,
    }
]

NO_RECOMMENDED_MARKER_QUESTIONS = [
    {
        "question": "Approximate transform now, or build the real primitive?",
        "header": "Transform",
        "options": [
            {"label": "Approximate, no new primitive", "description": "Cheap for now, ships this session."},
            {"label": "Real transform primitive", "description": "The proper fundamental fix."},
        ],
        "multiSelect": False,
    }
]


# ---- common.detect_shortcut_fork ----


def test_incident_shape_fires():
    hits = common.detect_shortcut_fork({"questions": INCIDENT_QUESTIONS})
    check("incident-shaped fork detected", len(hits) == 1, hits)


def test_destructive_confirmation_does_not_fire():
    hits = common.detect_shortcut_fork({"questions": DESTRUCTIVE_CONFIRM_QUESTIONS})
    check("destructive-action confirmation does not fire", hits == [], hits)


def test_taste_call_does_not_fire():
    hits = common.detect_shortcut_fork({"questions": TASTE_CALL_QUESTIONS})
    check("genuine taste call does not fire", hits == [], hits)


def test_multiselect_feature_choice_does_not_fire():
    hits = common.detect_shortcut_fork({"questions": MULTISELECT_FEATURE_QUESTIONS})
    check("multiSelect feature choice does not fire", hits == [], hits)


def test_recommended_is_root_fix_does_not_fire():
    hits = common.detect_shortcut_fork({"questions": RECOMMENDED_IS_ROOT_FIX_QUESTIONS})
    check("recommending the root fix itself does not fire", hits == [], hits)


def test_no_recommended_marker_does_not_fire():
    hits = common.detect_shortcut_fork({"questions": NO_RECOMMENDED_MARKER_QUESTIONS})
    check("no (Recommended) marker at all does not fire", hits == [], hits)


def test_single_option_question_does_not_fire():
    hits = common.detect_shortcut_fork(
        {"questions": [{"question": "ok?", "header": "h", "options": [{"label": "Only one (Recommended)", "description": "approximate for now"}]}]}
    )
    check("single-option question never fires", hits == [], hits)


def test_malformed_input_never_raises():
    for bad in (None, {}, {"questions": None}, {"questions": "not a list"}, {"questions": [None]}, {"questions": [{"options": "nope"}]}):
        try:
            hits = common.detect_shortcut_fork(bad)
            check(f"malformed input {bad!r} returns []", hits == [], hits)
        except Exception as e:
            check(f"malformed input {bad!r} does not raise", False, repr(e))


# ---- ask-question-guard.py main() ----


def test_hook_denies_incident_shape():
    def run(_td):
        out = run_hook({"session_id": "s1", "transcript_path": "/tmp/none.jsonl", "tool_input": {"questions": INCIDENT_QUESTIONS}})
        check("hook emits output for incident shape", out != "", out)
        if out:
            obj = json.loads(out)
            decision = obj.get("hookSpecificOutput", {}).get("permissionDecision")
            check("hook denies incident shape", decision == "deny", obj)

    with_bounce_dir(run)


def test_hook_bounces_once_then_allows():
    def run(_td):
        payload = {"session_id": "s2", "transcript_path": "/tmp/none.jsonl", "tool_input": {"questions": INCIDENT_QUESTIONS}}
        first = run_hook(payload)
        second = run_hook(payload)
        check("first ask is denied", first != "" and json.loads(first)["hookSpecificOutput"]["permissionDecision"] == "deny", first)
        check("identical re-ask is allowed through (no output)", second == "", second)

    with_bounce_dir(run)


def test_hook_distinct_question_denies_independently():
    def run(_td):
        first = run_hook({"session_id": "s3", "transcript_path": "/tmp/none.jsonl", "tool_input": {"questions": INCIDENT_QUESTIONS}})
        second = run_hook({"session_id": "s3", "transcript_path": "/tmp/none.jsonl", "tool_input": {"questions": RECOMMENDED_IS_ROOT_FIX_QUESTIONS}})
        check("first (incident) denies", first != "", first)
        check("a different, legitimate question is unaffected by the other's bounce marker", second == "", second)

    with_bounce_dir(run)


def test_hook_no_output_for_clean_question():
    def run(_td):
        out = run_hook({"session_id": "s4", "transcript_path": "/tmp/none.jsonl", "tool_input": {"questions": DESTRUCTIVE_CONFIRM_QUESTIONS}})
        check("clean question produces no output (allow)", out == "", out)

    with_bounce_dir(run)


def test_hook_malformed_stdin_fails_open():
    buf = io.StringIO()
    orig_stdin = sys.stdin
    sys.stdin = io.StringIO("not json at all {{{")
    try:
        with redirect_stdout(buf):
            hook.main()
    except Exception as e:
        check("malformed stdin never raises out of main()", False, repr(e))
    else:
        check("malformed stdin fails open with no output", buf.getvalue().strip() == "", buf.getvalue())
    finally:
        sys.stdin = orig_stdin


def test_hook_missing_tool_input_fails_open():
    out = run_hook({"session_id": "s5", "transcript_path": "/tmp/none.jsonl"})
    check("missing tool_input fails open", out == "", out)


def main():
    tests = [
        test_incident_shape_fires,
        test_destructive_confirmation_does_not_fire,
        test_taste_call_does_not_fire,
        test_multiselect_feature_choice_does_not_fire,
        test_recommended_is_root_fix_does_not_fire,
        test_no_recommended_marker_does_not_fire,
        test_single_option_question_does_not_fire,
        test_malformed_input_never_raises,
        test_hook_denies_incident_shape,
        test_hook_bounces_once_then_allows,
        test_hook_distinct_question_denies_independently,
        test_hook_no_output_for_clean_question,
        test_hook_malformed_stdin_fails_open,
        test_hook_missing_tool_input_fails_open,
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
