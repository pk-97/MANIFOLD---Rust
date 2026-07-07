#!/usr/bin/env python3
"""
Standalone test for the habit-memory extension (DESIGN.md §4c-2): at
observer start, roll up telemetry fires per move over the trailing 7 days
across all sessions; on delivery the valve appends the frozen template
"(Nth fire of this move across sessions this week.)" with only the ordinal
varying. Exercises Daemon._rollup_weekly_fires/_resolve_fire and
valve.build_block directly with synthetic input — no classifier calls.

Run: python3 .claude/daemon/test_habit_memory.py
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
import common  # noqa: E402

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


def write_telemetry(records):
    os.makedirs(os.path.dirname(valve.TELEMETRY_PATH), exist_ok=True)
    with open(valve.TELEMETRY_PATH, "w", encoding="utf-8") as f:
        for r in records:
            f.write(json.dumps(r) + "\n")


FAKE_VERDICT = {"evidence": "x", "confidence": 0.9}


def test_rollup_counts_scored_within_window():
    def run(td):
        now = time.time()
        write_telemetry(
            [
                {"ts": now - 1000, "event": "scored", "move_id": "anchor/verify-claim", "outcome": "success"},
                {"ts": now - 2000, "event": "scored", "move_id": "anchor/verify-claim", "outcome": "failure"},
                {"ts": now - 3000, "event": "scored", "move_id": "anchor/thrash", "outcome": "unscored"},
            ]
        )
        counts = observer.Daemon._rollup_weekly_fires()
        check("verify-claim rolled up to 2", counts.get("anchor/verify-claim") == 2, counts)
        check("thrash rolled up to 1", counts.get("anchor/thrash") == 1, counts)
    with_temp_dirs(run)


def test_rollup_excludes_records_older_than_7_days():
    def run(td):
        now = time.time()
        write_telemetry(
            [
                {"ts": now - 3600, "event": "scored", "move_id": "anchor/verify-claim", "outcome": "success"},
                {"ts": now - 8 * 86400, "event": "scored", "move_id": "anchor/verify-claim", "outcome": "success"},
            ]
        )
        counts = observer.Daemon._rollup_weekly_fires()
        check("only the recent record counted", counts.get("anchor/verify-claim") == 1, counts)
    with_temp_dirs(run)


def test_rollup_ignores_non_scored_events():
    def run(td):
        now = time.time()
        write_telemetry(
            [
                {"ts": now, "event": "injected", "move_id": "anchor/verify-claim"},
                {"ts": now, "event": "observer_spawn"},
            ]
        )
        counts = observer.Daemon._rollup_weekly_fires()
        check("no scored events -> empty rollup", counts == {}, counts)
    with_temp_dirs(run)


def test_resolve_fire_seeds_from_rollup_and_increments():
    def run(td):
        write_telemetry([{"ts": time.time(), "event": "scored", "move_id": "anchor/verify-claim", "outcome": "success"}])
        d = make_daemon()
        d.weekly_fire_counts = d._rollup_weekly_fires()
        logf = io.StringIO()
        r1 = d._resolve_fire(0, "anchor/verify-claim", FAKE_VERDICT, logf)
        r2 = d._resolve_fire(20, "anchor/verify-claim", FAKE_VERDICT, logf)
        check("1st fire this session continues from baseline 1 -> 2", r1["weekly_count"] == 2, r1)
        check("2nd fire this session -> 3", r2["weekly_count"] == 3, r2)
    with_temp_dirs(run)


def test_resolve_fire_weekly_count_none_for_agent_mailbox():
    """Original intent (worker fires get no weekly habit count) now splits
    across §2b observation-only mode (2026-07-07): corrective worker fires
    don't deliver at all — their shadow telemetry carries no weekly_count —
    and advice fires, which still deliver, keep weekly_count=None."""
    def run(td):
        d = make_daemon()
        worker = observer.AgentWorker("agent1", "test-session", "/dev/null")
        logf = io.StringIO()
        r = d._resolve_fire(0, "anchor/verify-claim", FAKE_VERDICT, logf, mailbox=worker)
        check("corrective agent-mailbox fire is shadowed, not delivered (§2b observation-only)", r is None, r)
        shadow = [json.loads(l) for l in open(valve.TELEMETRY_PATH)][-1]
        check("shadow record has no weekly_count (out of scope, like §4b)", shadow.get("event") == "worker_shadow_fire" and "weekly_count" not in shadow, shadow)
        r2 = d._resolve_fire(0, "mechanical/reasoning-primer", FAKE_VERDICT, logf, mailbox=worker)
        check("advice agent-mailbox fire still delivers with weekly_count=None", r2 is not None and r2["weekly_count"] is None, r2)
    with_temp_dirs(run)


def test_build_block_appends_frozen_template():
    flag = {"move_id": "anchor/verify-claim", "confidence": 0.9, "weekly_count": 3}
    block = valve.build_block(flag)
    check("block built", block is not None)
    check("frozen template text present verbatim", "fire of this move across sessions this week." in block, block)
    check("ordinal substituted correctly", "3rd fire of this move across sessions this week." in block, block)


def test_build_block_omits_habit_line_when_weekly_count_absent():
    flag = {"move_id": "anchor/verify-claim", "confidence": 0.9}
    block = valve.build_block(flag)
    check("no habit line when weekly_count missing", "fire of this move across sessions this week." not in block, block)


def test_build_block_omits_habit_line_when_weekly_count_none():
    flag = {"move_id": "anchor/verify-claim", "confidence": 0.9, "weekly_count": None}
    block = valve.build_block(flag)
    check("no habit line when weekly_count is None", "fire of this move across sessions this week." not in block, block)


def test_build_block_supervised_ack_names_the_fires_own_seq():
    # 2026-07-05 addressability fix: 55/74 self-graded records carried
    # seq:null because this sentence never told the session its own fire's
    # seq — the sleep pass couldn't join the grade back to the fire.
    flag = {"move_id": "anchor/verify-claim", "confidence": 0.9, "seq": 7}
    block = valve.build_block(flag)
    check("ack sentence names this fire's seq", "this fire: seq 7" in block, block)
    check('grade-line instruction includes "seq": 7 verbatim', '"seq": 7' in block, block)


def test_build_block_supervised_ack_seq_varies_with_the_flag():
    block5 = valve.build_block({"move_id": "anchor/verify-claim", "confidence": 0.9, "seq": 5})
    block12 = valve.build_block({"move_id": "anchor/verify-claim", "confidence": 0.9, "seq": 12})
    check("seq 5 fire names seq 5, not another", "this fire: seq 5" in block5 and "this fire: seq 12" not in block5, block5)
    check("seq 12 fire names seq 12, not another", "this fire: seq 12" in block12 and "this fire: seq 5" not in block12, block12)


# ---- DESIGN.md §2h.4: build_block's supervised ack must reach worker
# deliveries too, with the worker's agent_id folded into the grade-line
# instruction (RUNBOOK.md step 2 — plain (session_id, seq) collides across
# workers). ----


def test_build_block_worker_ack_includes_agent_id():
    flag = {"move_id": "anchor/verify-claim", "confidence": 0.9, "seq": 9}
    block = valve.build_block(flag, agent_id="worker-abc")
    check("worker ack still names this fire's own seq", "this fire: seq 9" in block, block)
    check('worker ack grade-line schema includes "agent_id"', '"agent_id": "worker-abc"' in block, block)


def test_build_block_main_ack_omits_agent_id_field():
    flag = {"move_id": "anchor/verify-claim", "confidence": 0.9, "seq": 9}
    block_default = valve.build_block(flag)
    block_explicit_none = valve.build_block(flag, agent_id=None)
    check("no agent_id in the main-session ack (default call)", '"agent_id"' not in block_default, block_default)
    check("no agent_id in the main-session ack (explicit agent_id=None)", '"agent_id"' not in block_explicit_none, block_explicit_none)


def test_build_block_advice_kind_never_carries_agent_id():
    # Advice-kind moves (§2e) never get the supervised ack at all — passing
    # agent_id must not change that; the advice frame has no ack to attach it to.
    flag = {"move_id": "mechanical/reasoning-primer", "confidence": 0.9, "seq": 9}
    block = valve.build_block(flag, agent_id="worker-abc")
    check("advice frame present", "<daemon-advice" in block, block)
    check("no supervised-ack or agent_id leaks into the advice frame", "agent_id" not in block, block)


def main():
    tests = [
        test_rollup_counts_scored_within_window,
        test_rollup_excludes_records_older_than_7_days,
        test_rollup_ignores_non_scored_events,
        test_resolve_fire_seeds_from_rollup_and_increments,
        test_resolve_fire_weekly_count_none_for_agent_mailbox,
        test_build_block_appends_frozen_template,
        test_build_block_omits_habit_line_when_weekly_count_absent,
        test_build_block_omits_habit_line_when_weekly_count_none,
        test_build_block_supervised_ack_names_the_fires_own_seq,
        test_build_block_supervised_ack_seq_varies_with_the_flag,
        test_build_block_worker_ack_includes_agent_id,
        test_build_block_main_ack_omits_agent_id_field,
        test_build_block_advice_kind_never_carries_agent_id,
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
