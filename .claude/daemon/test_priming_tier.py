"""Tests for the priming tier (sleep pass 1 + §2e advice tier):
mechanical/reasoning-primer (first live tool event, re-arming every
advice-recur events per target) and mechanical/unread-edit (Edit/MultiEdit
to a path never Read or Written this session).

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


# ---- reasoning-primer ----


def test_primer_first_fire_then_recur_gate():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_primer(1, logf)
        flag = read_flag(d)
        check("primer fired on first event", flag and flag["move_id"] == "mechanical/reasoning-primer", flag)
        seq1 = flag["seq"]
        # Deliver it, then verify the advice-recur gate holds inside the
        # window and re-arms past it (§2e).
        with open(d.consumed_path, "w", encoding="utf-8") as f:
            f.write(str(seq1))
        d._check_primer(2, logf)
        d._check_primer(250, logf)
        check("primer gated within advice-recur window", read_flag(d)["seq"] == seq1)
        d._check_primer(301, logf)
        check("primer re-fired after advice-recur gap", read_flag(d)["seq"] != seq1)
        check("fire_count is 2", d.fire_count.get("mechanical/reasoning-primer") == 2)

    with_temp_dirs(run)


def test_advice_recurs_and_never_escalates():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        seqs = []
        # Three fires would escalate any alert move (ESCALATE_AFTER=2);
        # advice-kind moves recur by design and must never checkpoint.
        for ev in (1, 301, 601):
            d._check_primer(ev, logf)
            flag = read_flag(d)
            check(f"primer fired at event {ev}", flag and flag["move_id"] == "mechanical/reasoning-primer", flag)
            seqs.append(flag["seq"])
            with open(d.consumed_path, "w", encoding="utf-8") as f:
                f.write(str(flag["seq"]))
        check("three distinct fires", len(set(seqs)) == 3, seqs)
        check("no escalation on 3rd advice fire", not d.escalated)

    with_temp_dirs(run)


def test_primer_retries_while_another_flag_pending():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        # A pending, undelivered flag occupies the one-whisper slot.
        d._check_stopgap("Edit", {"file_path": "a.rs", "old_string": "x", "new_string": "x // HACK"}, 1, logf)
        pending = read_flag(d)
        check("stopgap occupies the slot", pending["move_id"] == "mechanical/confessed-stopgap")
        d._check_primer(2, logf)
        check("primer suppressed while slot busy", read_flag(d)["seq"] == pending["seq"])
        check("primer cooldown not consumed", not d.fire_count.get("mechanical/reasoning-primer"))
        # Deliver the pending flag; primer succeeds on the next event.
        with open(d.consumed_path, "w", encoding="utf-8") as f:
            f.write(str(pending["seq"]))
        d._check_primer(3, logf)
        check("primer fired after slot freed", read_flag(d)["move_id"] == "mechanical/reasoning-primer")

    with_temp_dirs(run)


def test_primer_fires_per_worker_mailbox():
    def run(td):
        d = make_daemon()
        w = observer.AgentWorker("test-session", "agentx", "/dev/null")
        logf = io.StringIO()
        d._check_primer(1, logf, mailbox=w)
        with open(w.verdict_path, encoding="utf-8") as f:
            flag = json.load(f)["flag"]
        check("worker primer fired into worker mailbox", flag["move_id"] == "mechanical/reasoning-primer")
        check("main-session state untouched", not os.path.exists(d.verdict_path))

    with_temp_dirs(run)


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
