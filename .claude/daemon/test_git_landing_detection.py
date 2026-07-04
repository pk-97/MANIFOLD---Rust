#!/usr/bin/env python3
"""
Standalone test for .claude/GIT_TREE_DISCIPLINE.md §2's mechanical/git-landing
move: common.py's GIT_LANDING_MARKERS / detect_git_landing_signal, and
observer.py's deterministic fire path (_check_git_landing). Never spawns a
real classifier call — the mechanical fire path bypasses Haiku entirely, by
design, mirroring test_stopgap_detection.py's pattern for the sibling
mechanical/confessed-stopgap move.

Run: python3 .claude/daemon/test_git_landing_detection.py
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


# ---- common.detect_git_landing_signal ----


def test_cherry_pick_detected():
    hits = common.detect_git_landing_signal("Bash", {"command": "git cherry-pick abc123"})
    check("cherry-pick detected", "cherry-pick" in hits, hits)


def test_branch_delete_short_flag_detected():
    hits = common.detect_git_landing_signal("Bash", {"command": "git branch -d old-lane"})
    check("branch -d detected", "branch-delete" in hits, hits)


def test_branch_delete_capital_flag_detected():
    hits = common.detect_git_landing_signal("Bash", {"command": "git branch -D old-lane"})
    check("branch -D detected", "branch-delete" in hits, hits)


def test_branch_delete_long_flag_detected():
    hits = common.detect_git_landing_signal("Bash", {"command": "git branch --delete old-lane"})
    check("branch --delete detected", "branch-delete" in hits, hits)


def test_remote_branch_delete_detected():
    hits = common.detect_git_landing_signal("Bash", {"command": "git push origin --delete old-lane"})
    check("push --delete (remote branch delete) detected", "branch-delete" in hits, hits)


def test_plain_push_not_flagged():
    hits = common.detect_git_landing_signal("Bash", {"command": "git push origin my-branch"})
    check("plain push does not fire", hits == [], hits)


def test_plain_merge_not_flagged():
    hits = common.detect_git_landing_signal("Bash", {"command": "git merge some-branch"})
    check("plain merge does not fire", hits == [], hits)


def test_non_bash_tool_never_fires():
    hits = common.detect_git_landing_signal("Edit", {"file_path": "foo.rs", "new_string": "git cherry-pick abc123"})
    check("non-Bash tool never scanned", hits == [], hits)


def test_malformed_input_never_raises():
    hits = common.detect_git_landing_signal("Bash", "not-a-dict")
    check("malformed input returns empty, no raise", hits == [], hits)


def test_both_categories_can_fire_together():
    hits = common.detect_git_landing_signal(
        "Bash", {"command": "git cherry-pick abc123 && git branch -d old-lane"}
    )
    check("cherry-pick present", "cherry-pick" in hits, hits)
    check("branch-delete present", "branch-delete" in hits, hits)


# ---- observer.py: deterministic mechanical fire ----


def make_daemon():
    return observer.Daemon("test-session", "/dev/null")


def test_check_git_landing_fires_flag_for_cherry_pick():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_git_landing("Bash", {"command": "git cherry-pick abc123"}, event_count=1, logf=logf)
        with open(d.verdict_path, encoding="utf-8") as f:
            record = json.load(f)
        check("flag written", record.get("flag") is not None, record)
        check("move_id is mechanical/git-landing", record["flag"]["move_id"] == "mechanical/git-landing", record)
        check("window_version on record", record.get("window_version") == common.WINDOW_VERSION, record)
    with_temp_dirs(run)


def test_check_git_landing_fires_flag_for_branch_delete():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_git_landing("Bash", {"command": "git branch -D stale-lane"}, event_count=1, logf=logf)
        with open(d.verdict_path, encoding="utf-8") as f:
            record = json.load(f)
        check("move_id is mechanical/git-landing", record["flag"]["move_id"] == "mechanical/git-landing", record)
    with_temp_dirs(run)


def test_check_git_landing_no_hit_writes_nothing():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_git_landing("Bash", {"command": "git push origin my-branch"}, event_count=1, logf=logf)
        check("no verdict file written when clean", not os.path.exists(d.verdict_path))
    with_temp_dirs(run)


def test_check_git_landing_respects_cooldown():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        cmd = {"command": "git cherry-pick abc123"}
        d._check_git_landing("Bash", cmd, event_count=1, logf=logf)
        with open(d.verdict_path, encoding="utf-8") as f:
            first = json.load(f)
        d._check_git_landing("Bash", cmd, event_count=2, logf=logf)
        with open(d.verdict_path, encoding="utf-8") as f:
            second = json.load(f)
        check(
            "second fire within cooldown does not overwrite the undelivered flag",
            second["flag"] == first["flag"],
            (first, second),
        )
    with_temp_dirs(run)


def test_moves_md_has_git_landing_entry():
    moves_text = common.read(str(DAEMON_DIR / "moves.md"))
    moves = common.parse_moves(moves_text)
    check("mechanical/git-landing present in moves.md", "mechanical/git-landing" in moves, sorted(moves))
    entry = moves.get("mechanical/git-landing") or {}
    check("has a signature", bool(entry.get("signature")), entry)
    check("has a payload", bool(entry.get("payload")), entry)
    check("never classifier-selectable", common.validate_move_id("mechanical/git-landing", moves) is None)


def main():
    test_cherry_pick_detected()
    test_branch_delete_short_flag_detected()
    test_branch_delete_capital_flag_detected()
    test_branch_delete_long_flag_detected()
    test_remote_branch_delete_detected()
    test_plain_push_not_flagged()
    test_plain_merge_not_flagged()
    test_non_bash_tool_never_fires()
    test_malformed_input_never_raises()
    test_both_categories_can_fire_together()
    test_check_git_landing_fires_flag_for_cherry_pick()
    test_check_git_landing_fires_flag_for_branch_delete()
    test_check_git_landing_no_hit_writes_nothing()
    test_check_git_landing_respects_cooldown()
    test_moves_md_has_git_landing_entry()

    for name in PASS:
        print(f"PASS: {name}")
    for name, detail in FAIL:
        print(f"FAIL: {name} ({detail!r})")

    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
