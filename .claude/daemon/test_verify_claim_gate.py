#!/usr/bin/env python3
"""
Standalone test for the anchor/verify-claim class-(a) FP fix (DESIGN.md §2 /
PASS2_AGENDA item 3): common.contains_claim + WindowState.has_claim (the
deterministic claim-presence signal) and observer.py's _handle_window
suppression gate. The gate is a post-filter over a REAL classifier verdict, so
these tests stub common.call_classifier — never a real Haiku call — and stub
valve.append_telemetry so nothing lands in the live telemetry.jsonl (same
leak-prevention discipline as test_ask_question_guard.py's header).

Run: python3 .claude/daemon/test_verify_claim_gate.py
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


# ---- common.contains_claim ----


def test_contains_claim_true_for_completion_claims():
    for t in [
        "Done. The tests pass.",
        "Fixed the overlap bug.",
        "It works now.",
        "Implemented and pushed to main.",
        "The gradient renders correctly now.",
        "This should resolve the timing issue.",
        "All three sub-parts are complete.",
        "clippy is clean and the build passes",
    ]:
        check(f"claim detected: {t!r}", common.contains_claim([t]), t)


def test_contains_claim_false_for_recon_narration():
    for t in [
        "Let me read the fixtures file to understand the layout.",
        "I'll check how the scheduler handles this.",
        "Reading sync_clips_to_time to see what it does.",
        "What behavior are you seeing on stage?",
        "Waiting for the agent to finish before continuing.",
        "Looking at the ledger for the last few events.",
    ]:
        check(f"no claim in recon: {t!r}", not common.contains_claim([t]), t)


def test_contains_claim_scans_all_recent_texts():
    check("claim in any of the recent texts counts", common.contains_claim(["gathering context", "the fix works"]))
    check("no claim across all recent texts", not common.contains_claim(["gathering context", "reading files"]))


def test_contains_claim_never_raises_on_junk():
    check("empty iterable safe", common.contains_claim([]) is False)
    check("None safe", common.contains_claim(None) is False)
    check("non-str members ignored", common.contains_claim([None, 3, {"x": 1}]) is False)


# ---- WindowState.has_claim on the closed window ----


def test_close_window_sets_has_claim_true():
    state = common.WindowState()
    state.current_task = "fix the bug"
    state.recent_texts = ["The fix is in and the tests pass."]
    closed = state._close_window(1.0)
    check("has_claim True when RECENT asserts a claim", closed.get("has_claim") is True, closed)


def test_close_window_sets_has_claim_false():
    state = common.WindowState()
    state.current_task = "investigate the bug"
    state.recent_texts = ["Let me read sync_clips_to_time to understand the ordering."]
    closed = state._close_window(1.0)
    check("has_claim False for pure recon RECENT", closed.get("has_claim") is False, closed)


# ---- observer._handle_window suppression gate ----


def with_temp_dirs(fn):
    orig = (observer.VERDICTS_DIR, observer.WORKER_NUDGES_FLAG, valve.VERDICTS_DIR,
            valve.WORKER_NUDGES_FLAG, valve.TELEMETRY_PATH)
    with tempfile.TemporaryDirectory() as td:
        observer.VERDICTS_DIR = td
        observer.WORKER_NUDGES_FLAG = os.path.join(td, "worker-nudges.enabled")
        valve.VERDICTS_DIR = td
        valve.WORKER_NUDGES_FLAG = os.path.join(td, "worker-nudges.enabled")
        valve.TELEMETRY_PATH = os.path.join(td, "telemetry.jsonl")
        try:
            fn(td)
        finally:
            (observer.VERDICTS_DIR, observer.WORKER_NUDGES_FLAG, valve.VERDICTS_DIR,
             valve.WORKER_NUDGES_FLAG, valve.TELEMETRY_PATH) = orig


TELEMETRY = []


def _install_classifier(flag):
    """Stub common.call_classifier to return a verdict flagging `flag`."""
    common.call_classifier = lambda sp, wt, *a, **kw: {
        "flag": flag, "phase": "reporting", "evidence": "stub", "confidence": 0.9,
    }


def _install_telemetry_capture():
    TELEMETRY.clear()
    valve.append_telemetry = lambda rec: TELEMETRY.append(rec)


def make_daemon():
    return observer.Daemon("test-session", "/dev/null")


def _window(has_claim, end=5):
    w = {"end_event_count": end, "end_ts": float(end), "text": "TASK: x\n\nRECENT:\nstub"}
    if has_claim is not None:
        w["has_claim"] = has_claim
    return w


def test_verify_claim_suppressed_when_no_claim():
    def run(td):
        _install_classifier("anchor/verify-claim")
        _install_telemetry_capture()
        d = make_daemon()
        d._handle_window(_window(has_claim=False), io.StringIO())
        with open(d.verdict_path, encoding="utf-8") as f:
            record = json.load(f)
        check("no verify-claim flag delivered when window has no claim", record.get("flag") in (None,), record)
        suppressions = [r for r in TELEMETRY if r.get("event") == "verify_claim_suppressed"]
        check("suppression telemetry emitted", len(suppressions) == 1, TELEMETRY)
        check("suppression reason is no-claim-in-window",
              suppressions and suppressions[0].get("reason") == "no-claim-in-window", suppressions)
    with_temp_dirs(run)


def test_verify_claim_fires_when_claim_present():
    def run(td):
        _install_classifier("anchor/verify-claim")
        _install_telemetry_capture()
        d = make_daemon()
        d._handle_window(_window(has_claim=True), io.StringIO())
        with open(d.verdict_path, encoding="utf-8") as f:
            record = json.load(f)
        check("verify-claim fires when a claim is in the window",
              (record.get("flag") or {}).get("move_id") == "anchor/verify-claim", record)
        check("no suppression telemetry when a claim is present",
              not [r for r in TELEMETRY if r.get("event") == "verify_claim_suppressed"], TELEMETRY)
    with_temp_dirs(run)


def test_legacy_window_without_has_claim_field_is_not_suppressed():
    # A window that predates the field (default True) must never be suppressed —
    # the gate only ever removes a fire on a window it KNOWS carries no claim.
    def run(td):
        _install_classifier("anchor/verify-claim")
        _install_telemetry_capture()
        d = make_daemon()
        d._handle_window(_window(has_claim=None), io.StringIO())
        with open(d.verdict_path, encoding="utf-8") as f:
            record = json.load(f)
        check("no-field window fires (default True)",
              (record.get("flag") or {}).get("move_id") == "anchor/verify-claim", record)
    with_temp_dirs(run)


def test_other_move_not_gated_by_has_claim():
    # The gate is verify-claim-specific: another classifier-selectable move must
    # fire regardless of has_claim.
    def run(td):
        other = next(
            m for m in observer.Daemon("s", "/dev/null").moves
            if common.validate_move_id(m, make_daemon().moves) and m != "anchor/verify-claim"
        )
        _install_classifier(other)
        _install_telemetry_capture()
        d = make_daemon()
        d._handle_window(_window(has_claim=False), io.StringIO())
        with open(d.verdict_path, encoding="utf-8") as f:
            record = json.load(f)
        check(f"{other} fires even with has_claim False",
              (record.get("flag") or {}).get("move_id") == other, record)
        check("no verify-claim suppression telemetry for a different move",
              not [r for r in TELEMETRY if r.get("event") == "verify_claim_suppressed"], TELEMETRY)
    with_temp_dirs(run)


def main():
    test_contains_claim_true_for_completion_claims()
    test_contains_claim_false_for_recon_narration()
    test_contains_claim_scans_all_recent_texts()
    test_contains_claim_never_raises_on_junk()
    test_close_window_sets_has_claim_true()
    test_close_window_sets_has_claim_false()
    test_verify_claim_suppressed_when_no_claim()
    test_verify_claim_fires_when_claim_present()
    test_legacy_window_without_has_claim_field_is_not_suppressed()
    test_other_move_not_gated_by_has_claim()

    for name in PASS:
        print(f"PASS: {name}")
    for name, detail in FAIL:
        print(f"FAIL: {name} ({detail!r})")
    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
