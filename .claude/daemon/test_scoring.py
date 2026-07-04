#!/usr/bin/env python3
"""
Standalone test for observer.py's outcome-scoring + auto-mute logic
(DESIGN.md §4b). Exercises the Daemon methods directly with a monkeypatched
verdicts/telemetry dir under a temp dir — never touches the real
`.claude/daemon/verdicts/` or `telemetry.jsonl`, and never spawns a classifier
call (the classifier is only invoked from `_handle_window`, which none of
these tests call).

Run: python3 .claude/daemon/test_scoring.py
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

spec = importlib.util.spec_from_file_location("observer", DAEMON_DIR / "observer.py")
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)
import valve  # noqa: E402  (already on sys.path via DAEMON_DIR insert above)

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


def make_daemon(consumed=0):
    d = observer.Daemon("test-session", "/dev/null")
    with open(d.consumed_path, "w", encoding="utf-8") as f:
        f.write(str(consumed))
    return d


def with_temp_dirs(fn):
    orig_verdicts, orig_mutes = observer.VERDICTS_DIR, observer.MUTES_DIR
    orig_valve_verdicts, orig_telemetry = valve.VERDICTS_DIR, valve.TELEMETRY_PATH
    with tempfile.TemporaryDirectory() as td:
        observer.VERDICTS_DIR = td
        observer.MUTES_DIR = os.path.join(td, "mutes")
        valve.VERDICTS_DIR = td
        valve.TELEMETRY_PATH = os.path.join(td, "telemetry.jsonl")
        try:
            fn(td)
        finally:
            observer.VERDICTS_DIR, observer.MUTES_DIR = orig_verdicts, orig_mutes
            valve.VERDICTS_DIR, valve.TELEMETRY_PATH = orig_valve_verdicts, orig_telemetry


def read_telemetry():
    if not os.path.exists(valve.TELEMETRY_PATH):
        return []
    with open(valve.TELEMETRY_PATH, encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def test_verify_claim_success():
    def run(td):
        d = make_daemon(consumed=1)
        d.fire_records[1] = "anchor/verify-claim"
        logf = io.StringIO()
        d._check_deliveries(logf)
        check("verify-claim: registered pending", 1 in d.pending_scores)
        d.recent_events.append((1, "Bash", "ok", "cargo test manifold-renderer"))
        d._score_pending(logf)
        recs = [r for r in read_telemetry() if r.get("event") == "scored"]
        check("verify-claim: success scored", any(r["outcome"] == "success" for r in recs), recs)
        check("verify-claim: pending cleared", 1 not in d.pending_scores)
    with_temp_dirs(run)


def test_verify_claim_failure_after_window():
    def run(td):
        d = make_daemon(consumed=1)
        d.fire_records[1] = "anchor/verify-claim"
        logf = io.StringIO()
        d._check_deliveries(logf)
        for i in range(10):
            d.recent_events.append((i + 1, "Read", "ok", "some_file.rs"))
        d._score_pending(logf)
        recs = [r for r in read_telemetry() if r.get("event") == "scored"]
        check("verify-claim: failure after 10 no-signal events", any(r["outcome"] == "failure" for r in recs), recs)
    with_temp_dirs(run)


def test_verify_claim_not_enough_events_yet():
    def run(td):
        d = make_daemon(consumed=1)
        d.fire_records[1] = "anchor/verify-claim"
        logf = io.StringIO()
        d._check_deliveries(logf)
        for i in range(3):
            d.recent_events.append((i + 1, "Read", "ok", "some_file.rs"))
        d._score_pending(logf)
        check("verify-claim: still pending under 10 events, no signal", 1 in d.pending_scores)
        recs = [r for r in read_telemetry() if r.get("event") == "scored"]
        check("verify-claim: no premature score", not recs, recs)
    with_temp_dirs(run)


def test_thrash_success():
    def run(td):
        d = make_daemon(consumed=1)
        d.fire_records[1] = "anchor/thrash"
        logf = io.StringIO()
        d._check_deliveries(logf)
        d.recent_events.append((1, "Bash", "err", "cargo build"))
        d.recent_events.append((2, "Bash", "err", "cargo build"))
        d.recent_events.append((3, "Bash", "ok", "cargo build"))
        d._score_pending(logf)
        recs = [r for r in read_telemetry() if r.get("event") == "scored"]
        check("thrash: success once error streak ends", any(r["outcome"] == "success" for r in recs), recs)
    with_temp_dirs(run)


def test_thrash_failure():
    def run(td):
        d = make_daemon(consumed=1)
        d.fire_records[1] = "anchor/thrash"
        logf = io.StringIO()
        d._check_deliveries(logf)
        for i in range(10):
            d.recent_events.append((i + 1, "Bash", "err", "cargo build"))
        d._score_pending(logf)
        recs = [r for r in read_telemetry() if r.get("event") == "scored"]
        check("thrash: failure when streak never ends", any(r["outcome"] == "failure" for r in recs), recs)
    with_temp_dirs(run)


def test_circling_success():
    def run(td):
        d = make_daemon(consumed=1)
        d.recent_events.append((0, "Read", "ok", "foo.rs"))  # baseline before delivery
        d.fire_records[1] = "anchor/circling"
        logf = io.StringIO()
        d._check_deliveries(logf)
        check("circling: baseline captured", d.pending_scores[1]["baseline_tool_class"] == "Read")
        d.recent_events.append((1, "Bash", "ok", "cargo test"))
        d._score_pending(logf)
        recs = [r for r in read_telemetry() if r.get("event") == "scored"]
        check("circling: success on different tool class", any(r["outcome"] == "success" for r in recs), recs)
    with_temp_dirs(run)


def test_circling_failure():
    def run(td):
        d = make_daemon(consumed=1)
        d.recent_events.append((0, "Read", "ok", "foo.rs"))
        d.fire_records[1] = "anchor/circling"
        logf = io.StringIO()
        d._check_deliveries(logf)
        for i in range(10):
            d.recent_events.append((i + 1, "Read", "ok", "foo.rs"))
        d._score_pending(logf)
        recs = [r for r in read_telemetry() if r.get("event") == "scored"]
        check("circling: failure when tool class never changes", any(r["outcome"] == "failure" for r in recs), recs)
    with_temp_dirs(run)


def test_unscored_family():
    def run(td):
        d = make_daemon(consumed=1)
        d.fire_records[1] = "coaching/define-done"
        logf = io.StringIO()
        d._check_deliveries(logf)
        check("unscored: not added to pending (no mechanical signal)", 1 not in d.pending_scores)
        recs = [r for r in read_telemetry() if r.get("event") == "scored"]
        check("unscored: scored immediately as unscored", any(r["outcome"] == "unscored" for r in recs), recs)
    with_temp_dirs(run)


def test_auto_mute_triggers_at_threshold():
    def run(td):
        # Pre-seed 4 prior failures for this move_id on disk (simulating a
        # different observer instance / prior session).
        os.makedirs(os.path.dirname(valve.TELEMETRY_PATH), exist_ok=True)
        with open(valve.TELEMETRY_PATH, "w", encoding="utf-8") as f:
            for _ in range(4):
                f.write(json.dumps({"event": "scored", "move_id": "anchor/thrash", "outcome": "failure"}) + "\n")
        d = make_daemon(consumed=1)
        d.fire_records[1] = "anchor/thrash"
        logf = io.StringIO()
        d._check_deliveries(logf)
        for i in range(10):
            d.recent_events.append((i + 1, "Bash", "err", "cargo build"))
        d._score_pending(logf)  # the 5th failure -> should cross the mute threshold
        check("auto-mute: mute file created at 5th scored failure", d._is_muted("anchor/thrash"))
        mute_recs = [r for r in read_telemetry() if r.get("event") == "auto_mute"]
        check("auto-mute: auto_mute telemetry event logged", len(mute_recs) == 1, mute_recs)

        # A subsequent fire attempt for the muted move must be suppressed.
        result = d._resolve_fire(999, "anchor/thrash", {"evidence": "x", "confidence": 0.9}, logf)
        check("auto-mute: resolve_fire suppressed while muted", result is None)
    with_temp_dirs(run)


def test_auto_mute_does_not_trigger_with_a_success():
    def run(td):
        os.makedirs(os.path.dirname(valve.TELEMETRY_PATH), exist_ok=True)
        with open(valve.TELEMETRY_PATH, "w", encoding="utf-8") as f:
            for _ in range(4):
                f.write(json.dumps({"event": "scored", "move_id": "anchor/verify-claim", "outcome": "failure"}) + "\n")
            f.write(json.dumps({"event": "scored", "move_id": "anchor/verify-claim", "outcome": "success"}) + "\n")
        d = make_daemon(consumed=1)
        d.fire_records[1] = "anchor/verify-claim"
        logf = io.StringIO()
        d._check_deliveries(logf)
        for i in range(10):
            d.recent_events.append((i + 1, "Read", "ok", "foo.rs"))
        d._score_pending(logf)  # 6th scored, but one prior success exists
        check("auto-mute: not muted when a success exists in history", not d._is_muted("anchor/verify-claim"))
    with_temp_dirs(run)


def test_expired_mute_treated_as_absent():
    def run(td):
        d = make_daemon(consumed=0)
        os.makedirs(observer.MUTES_DIR, exist_ok=True)
        path = d._mute_path("anchor/circling")
        with open(path, "w", encoding="utf-8") as f:
            json.dump({"move_id": "anchor/circling", "unmute_at": 1.0}, f)  # long expired
        check("expired mute: is_muted returns False", not d._is_muted("anchor/circling"))
        check("expired mute: file cleaned up", not os.path.exists(path))
    with_temp_dirs(run)


def main():
    tests = [
        test_verify_claim_success,
        test_verify_claim_failure_after_window,
        test_verify_claim_not_enough_events_yet,
        test_thrash_success,
        test_thrash_failure,
        test_circling_success,
        test_circling_failure,
        test_unscored_family,
        test_auto_mute_triggers_at_threshold,
        test_auto_mute_does_not_trigger_with_a_success,
        test_expired_mute_treated_as_absent,
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
