"""Tests for the priming tier (sleep pass 1 + §2e advice tier):
mechanical/design-primer (live write of a design doc, re-arming every
advice-recur events per target) and mechanical/unread-edit (Edit/MultiEdit
to a path never Read or Written this session).

mechanical/reasoning-primer moved OUT of this tier 2026-07-15 (Peter's
ruling — DESIGN.md §2k): it no longer fires from the observer at all; it
now delivers once at SessionStart (see test_session_start.py). The
`_check_primer` tests that used to live here were removed with it.

Run: python3 test_priming_tier.py
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


def make_daemon():
    return observer.Daemon("test-session", "/dev/null")


def read_flag(d):
    with open(d.verdict_path, encoding="utf-8") as f:
        return json.load(f).get("flag")


# ---- unread-edit ----


def test_unread_edit_fires_on_never_seen_path():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_unread_edit("Edit", {"file_path": "crates/foo/src/lib.rs"}, 1, logf)
        flag = read_flag(d)
        check("unread edit fired", flag and flag["move_id"] == "mechanical/unread-edit", flag)

    with_temp_dirs(run)


def test_read_then_edit_does_not_fire():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_unread_edit("Read", {"file_path": "crates/foo/src/lib.rs"}, 1, logf)
        d._check_unread_edit("Edit", {"file_path": "crates/foo/src/lib.rs"}, 2, logf)
        check("read-then-edit is clean", not os.path.exists(d.verdict_path))

    with_temp_dirs(run)


def test_write_then_edit_does_not_fire():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_unread_edit("Write", {"file_path": "crates/foo/src/new.rs"}, 1, logf)
        d._check_unread_edit("Edit", {"file_path": "crates/foo/src/new.rs"}, 2, logf)
        check("write-then-edit is clean (authored file)", not os.path.exists(d.verdict_path))

    with_temp_dirs(run)


def test_catchup_populates_but_never_fires():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        # Catchup replay of an Edit (live=False): must not fire, must record the path.
        d._check_unread_edit("Edit", {"file_path": "crates/foo/src/lib.rs"}, 1, logf, live=False)
        check("no fire during catchup", not os.path.exists(d.verdict_path))
        # A later LIVE edit of the same path is now seen — still no fire.
        d._check_unread_edit("Edit", {"file_path": "crates/foo/src/lib.rs"}, 2, logf, live=True)
        check("catchup-seen path stays clean live", not os.path.exists(d.verdict_path))

    with_temp_dirs(run)


def test_excluded_paths_do_not_fire():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_unread_edit("Edit", {"file_path": "docs/NOTES.md"}, 1, logf)
        d._check_unread_edit("Edit", {"file_path": ".claude/daemon/moves.md"}, 2, logf)
        check("md and .claude excluded", not os.path.exists(d.verdict_path))

    with_temp_dirs(run)


def test_edit_does_not_vouch_for_itself():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_unread_edit("Edit", {"file_path": "crates/a.rs"}, 1, logf)
        flag1 = read_flag(d)
        check("first edit fired", flag1 and flag1["move_id"] == "mechanical/unread-edit")
        # Path is now seen (check-then-add): a second edit is clean even
        # though the first fire is still undelivered.
        with open(d.consumed_path, "w", encoding="utf-8") as f:
            f.write(str(flag1["seq"]))
        d._check_unread_edit("Edit", {"file_path": "crates/a.rs"}, 30, logf)
        check("second edit of same path clean", read_flag(d)["seq"] == flag1["seq"])

    with_temp_dirs(run)


def test_worker_paths_are_isolated():
    def run(td):
        d = make_daemon()
        w = observer.AgentWorker("test-session", "agentx", "/dev/null")
        logf = io.StringIO()
        # Session reads the file; the WORKER never did — worker edit fires.
        d._check_unread_edit("Read", {"file_path": "crates/a.rs"}, 1, logf)
        d._check_unread_edit("Edit", {"file_path": "crates/a.rs"}, 2, logf, mailbox=w)
        with open(w.verdict_path, encoding="utf-8") as f:
            flag = json.load(f)["flag"]
        check("worker fires on its own unseen path", flag["move_id"] == "mechanical/unread-edit")

    with_temp_dirs(run)


def test_moves_catalog_has_all_payloads():
    moves = common.parse_moves(common.read(os.path.join(DAEMON_DIR, "moves.md")))
    for mid in ("mechanical/reasoning-primer", "mechanical/unread-edit", "mechanical/design-primer"):
        entry = moves.get(mid) or {}
        check(f"{mid} in catalog with payload", bool(entry.get("payload")), mid)
    check("primer cooldown is advice-recur", (moves.get("mechanical/reasoning-primer") or {}).get("cooldown") == "advice-recur")
    check("design-primer cooldown is advice-recur", (moves.get("mechanical/design-primer") or {}).get("cooldown") == "advice-recur")
    check("primer kind is advice", (moves.get("mechanical/reasoning-primer") or {}).get("kind") == "advice")
    check("design-primer kind is advice", (moves.get("mechanical/design-primer") or {}).get("kind") == "advice")
    check("anchor kind defaults to alert", (moves.get("anchor/verify-claim") or {}).get("kind") == "alert")


def test_check_primer_removed_from_observer():
    """2026-07-15: mechanical/reasoning-primer's observer-side fire
    (`_check_primer`) was deleted outright, not just disarmed — it delivers
    solely via SessionStart now (test_session_start.py). This guards against
    a re-add that quietly restores the too-early firing Peter ruled out."""
    check("Daemon has no _check_primer method", not hasattr(observer.Daemon, "_check_primer"))


def test_build_block_advice_wrapper():
    block = valve.build_block({"move_id": "mechanical/reasoning-primer", "weekly_count": 5})
    check("advice tag", block is not None and block.startswith('<daemon-advice move="mechanical/reasoning-primer">'), block[:80] if block else block)
    check("advice closes with matching tag", block.rstrip().endswith("</daemon-advice>"))
    check("advice preamble present", "not a detection" in block)
    check("no supervised-mode ack in advice", "Supervised mode" not in block)
    check("no habit ordinal in advice", "fire of this move across sessions" not in block)
    check("payload present", "How to work, from the model that wrote this system" in block)


def test_build_block_alert_unchanged():
    block = valve.build_block({"move_id": "anchor/verify-claim"})
    check("alert tag unchanged", block is not None and block.startswith('<daemon move="anchor/verify-claim">'), block[:80] if block else block)
    check("alert keeps supervised-mode ack", "Supervised mode" in block)


# ---- design-primer ----


def test_design_primer_fires_on_design_doc_write():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_design_primer("Write", {"file_path": "docs/FOO_DESIGN.md"}, 1, logf)
        flag = read_flag(d)
        check("design primer fired", flag and flag["move_id"] == "mechanical/design-primer", flag)
        seq1 = flag["seq"]
        with open(d.consumed_path, "w", encoding="utf-8") as f:
            f.write(str(seq1))
        d._check_design_primer("Edit", {"file_path": "docs/BAR_PLAN.md"}, 2, logf)
        check("design primer gated within advice-recur window", read_flag(d)["seq"] == seq1)
        d._check_design_primer("Edit", {"file_path": "docs/BAR_PLAN.md"}, 400, logf)
        check("design primer re-fired after advice-recur gap", read_flag(d)["seq"] != seq1)

    with_temp_dirs(run)


def test_design_primer_ignores_non_design_paths():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_design_primer("Write", {"file_path": "docs/README.md"}, 1, logf)
        d._check_design_primer("Edit", {"file_path": "crates/foo/src/lib.rs"}, 2, logf)
        d._check_design_primer("Read", {"file_path": "docs/FOO_DESIGN.md"}, 3, logf)
        check("no fire on non-design writes or design reads", not os.path.exists(d.verdict_path))

    with_temp_dirs(run)


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
    print(f"\n{PASSED} passed, {FAILED} failed")
    sys.exit(1 if FAILED else 0)
