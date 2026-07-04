#!/usr/bin/env python3
"""
Standalone test for observer.py's fatigue-ordinal tracking (DESIGN.md
§4c-3b): every scored injection records the nth-fire ordinal of its move
within the session. Exercises Daemon._resolve_fire/_check_deliveries/
_score_pending directly with synthetic input — no classifier calls, no
watching this session's own transcript.

Run: python3 .claude/daemon/test_fatigue_ordinal.py
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
import valve  # noqa: E402

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


FAKE_VERDICT = {"evidence": "x", "confidence": 0.9}


def test_ordinal_increments_within_session():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        r1 = d._resolve_fire(0, "anchor/verify-claim", FAKE_VERDICT, logf)
        r2 = d._resolve_fire(20, "anchor/verify-claim", FAKE_VERDICT, logf)
        check("1st fire ordinal=1", d.fire_ordinals[r1["seq"]] == 1, d.fire_ordinals)
        check("2nd fire ordinal=2", d.fire_ordinals[r2["seq"]] == 2, d.fire_ordinals)
    with_temp_dirs(run)


def test_ordinal_independent_per_move():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        r1 = d._resolve_fire(0, "anchor/verify-claim", FAKE_VERDICT, logf)
        r2 = d._resolve_fire(0, "anchor/thrash", FAKE_VERDICT, logf)
        check("different moves each start at ordinal 1", d.fire_ordinals[r1["seq"]] == 1 and d.fire_ordinals[r2["seq"]] == 1)
    with_temp_dirs(run)


def test_escalation_gets_its_own_ordinal():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        r1 = d._resolve_fire(0, "anchor/verify-claim", FAKE_VERDICT, logf)
        r2 = d._resolve_fire(20, "anchor/verify-claim", FAKE_VERDICT, logf)
        r3 = d._resolve_fire(40, "anchor/verify-claim", FAKE_VERDICT, logf)
        check("3rd fire escalates", r3["move_id"] == "escalate/checkpoint", r3)
        check("escalated fire's own ordinal is 1 (first time it fires)", d.fire_ordinals[r3["seq"]] == 1, d.fire_ordinals)
        check(
            "base move's ordinal counter unaffected by the escalated fire",
            d.fire_ordinal["anchor/verify-claim"] == 2,
            d.fire_ordinal,
        )
        check("r1/r2 still ordinals 1/2", d.fire_ordinals[r1["seq"]] == 1 and d.fire_ordinals[r2["seq"]] == 2)
    with_temp_dirs(run)


def test_ordinal_in_scored_telemetry_for_scored_family():
    def run(td):
        d = make_daemon(consumed=0)
        logf = io.StringIO()
        d._resolve_fire(0, "anchor/verify-claim", FAKE_VERDICT, logf)
        with open(d.consumed_path, "w", encoding="utf-8") as f:
            f.write("1")
        d._check_deliveries(logf)
        check("pending score carries ordinal", d.pending_scores[1]["ordinal"] == 1, d.pending_scores)
        d.recent_events.append((1, "Bash", "ok", "cargo test manifold-renderer"))
        d._score_pending(logf)
        recs = [r for r in read_telemetry() if r.get("event") == "scored"]
        check("scored telemetry carries ordinal=1", any(r.get("ordinal") == 1 for r in recs), recs)
    with_temp_dirs(run)


def test_ordinal_in_scored_telemetry_for_unscored_family():
    def run(td):
        d = make_daemon(consumed=0)
        logf = io.StringIO()
        d._resolve_fire(0, "coaching/predict-before-look", FAKE_VERDICT, logf)
        d._resolve_fire(20, "coaching/predict-before-look", FAKE_VERDICT, logf)
        with open(d.consumed_path, "w", encoding="utf-8") as f:
            f.write("2")
        d._check_deliveries(logf)
        recs = [r for r in read_telemetry() if r.get("event") == "scored"]
        check("unscored family: two records, ordinals 1 and 2", sorted(r.get("ordinal") for r in recs) == [1, 2], recs)
    with_temp_dirs(run)


def test_ordinal_none_when_fire_predates_this_instance():
    """A fire recorded by a PRIOR observer instance (before an idle-exit
    revive) has no entry in this instance's fire_ordinals — must degrade to
    None, not crash or guess."""
    def run(td):
        d = make_daemon(consumed=1)
        d.fire_records[1] = "anchor/verify-claim"  # no matching fire_ordinals[1]
        logf = io.StringIO()
        d._check_deliveries(logf)
        check("ordinal is None, not KeyError", d.pending_scores[1]["ordinal"] is None)
    with_temp_dirs(run)


def main():
    tests = [
        test_ordinal_increments_within_session,
        test_ordinal_independent_per_move,
        test_escalation_gets_its_own_ordinal,
        test_ordinal_in_scored_telemetry_for_scored_family,
        test_ordinal_in_scored_telemetry_for_unscored_family,
        test_ordinal_none_when_fire_predates_this_instance,
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
