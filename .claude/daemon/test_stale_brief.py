#!/usr/bin/env python3
"""
Standalone test for TICKETS.md T9's mechanical/stale-brief (advice tier):
common.py's is_stale_brief_path / STALE_BRIEF_MAX_AGE_S and observer.py's
deterministic advice fire path (_check_stale_brief). Never spawns a real
classifier call — the mechanical fire path bypasses Haiku entirely, by
design, mirroring test_git_landing_detection.py's pattern for the sibling
mechanical moves.

Run: python3 .claude/daemon/test_stale_brief.py
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


# ---- common.is_stale_brief_path ----


def test_queue_path_matches():
    check("*_QUEUE.md matches", common.is_stale_brief_path("docs/FOO_QUEUE.md"))


def test_brief_path_matches():
    check("*BRIEF*.md matches", common.is_stale_brief_path("DESIGN_BRIEF_v2.md"))


def test_pass_agenda_path_matches():
    check("PASS*_AGENDA.md matches", common.is_stale_brief_path("PASS2_AGENDA.md"))


def test_docs_handoff_path_matches():
    check("docs/handoff* matches", common.is_stale_brief_path("docs/handoff_fable.md"))


def test_memory_handoff_path_matches():
    check("handoff_*.md matches", common.is_stale_brief_path("handoff_fable_window_2026_07.md"))


def test_unrelated_path_does_not_match():
    check("unrelated path does not match", not common.is_stale_brief_path("crates/foo/src/lib.rs"))


def test_empty_path_does_not_match():
    check("empty path does not match", not common.is_stale_brief_path(""))
    check("None path does not match", not common.is_stale_brief_path(None))


# ---- observer.py: deterministic advice fire ----


def make_daemon():
    return observer.Daemon("test-session", "/dev/null")


def _make_stale_file(td, name="FOO_QUEUE.md", age_hours=72):
    path = os.path.join(td, name)
    with open(path, "w", encoding="utf-8") as f:
        f.write("stale brief content")
    old_time = time.time() - age_hours * 3600
    os.utime(path, (old_time, old_time))
    return path


def test_check_stale_brief_fires_when_old():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        path = _make_stale_file(td, age_hours=72)
        d._check_stale_brief("Read", {"file_path": path}, event_count=1, logf=logf)
        with open(d.verdict_path, encoding="utf-8") as f:
            record = json.load(f)
        check("flag written", record.get("flag") is not None, record)
        check("move_id is mechanical/stale-brief", record["flag"]["move_id"] == "mechanical/stale-brief", record)
        check("window_version on record", record.get("window_version") == common.WINDOW_VERSION, record)
    with_temp_dirs(run)


def test_check_stale_brief_does_not_fire_when_recent():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        path = os.path.join(td, "FOO_QUEUE.md")
        with open(path, "w", encoding="utf-8") as f:
            f.write("fresh brief content")
        d._check_stale_brief("Read", {"file_path": path}, event_count=1, logf=logf)
        check("no verdict file written for a recent file", not os.path.exists(d.verdict_path))
    with_temp_dirs(run)


def test_check_stale_brief_fires_once_per_path():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        path = _make_stale_file(td, age_hours=72)
        d._check_stale_brief("Read", {"file_path": path}, event_count=1, logf=logf)
        with open(d.verdict_path, encoding="utf-8") as f:
            first = json.load(f)
        # Consume the first flag so a second fire wouldn't be suppressed by
        # the "one live flag at a time" rule instead of the per-path set /
        # the move's own "once" cooldown (mirrors
        # test_check_stopgap_respects_cooldown's pattern).
        with open(d.consumed_path, "w", encoding="utf-8") as f:
            f.write(str(first["flag"]["seq"]))
        d._check_stale_brief("Read", {"file_path": path}, event_count=2, logf=logf)
        with open(d.verdict_path, encoding="utf-8") as f:
            second = json.load(f)
        check(
            "second read of the same stale path does not re-fire (seq unchanged)",
            second["flag"]["seq"] == first["flag"]["seq"],
            (first, second),
        )
    with_temp_dirs(run)


def test_non_read_tool_never_fires():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        path = _make_stale_file(td, age_hours=72)
        d._check_stale_brief("Edit", {"file_path": path, "old_string": "x", "new_string": "y"}, event_count=1, logf=logf)
        check("non-Read tool never fires", not os.path.exists(d.verdict_path))
    with_temp_dirs(run)


def test_read_of_non_matching_path_never_fires():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        path = os.path.join(td, "lib.rs")
        with open(path, "w", encoding="utf-8") as f:
            f.write("fn main() {}")
        old_time = time.time() - 72 * 3600
        os.utime(path, (old_time, old_time))
        d._check_stale_brief("Read", {"file_path": path}, event_count=1, logf=logf)
        check("Read of a non-matching path never fires", not os.path.exists(d.verdict_path))
    with_temp_dirs(run)


def test_malformed_input_never_raises():
    d = make_daemon()
    logf = io.StringIO()
    try:
        d._check_stale_brief("Read", "not-a-dict", event_count=1, logf=logf)
        d._check_stale_brief("Read", {"file_path": "/nonexistent/FOO_QUEUE.md"}, event_count=1, logf=logf)
        ok = True
    except Exception:
        ok = False
    check("malformed/missing-file input never raises", ok)


def test_moves_md_has_stale_brief_entry():
    moves_text = common.read(str(DAEMON_DIR / "moves.md"))
    moves = common.parse_moves(moves_text)
    check("mechanical/stale-brief present in moves.md", "mechanical/stale-brief" in moves, sorted(moves))
    entry = moves.get("mechanical/stale-brief") or {}
    check("has a signature", bool(entry.get("signature")), entry)
    check("has a payload", bool(entry.get("payload")), entry)
    check("kind is advice", entry.get("kind") == "advice", entry)
    check("never classifier-selectable", common.validate_move_id("mechanical/stale-brief", moves) is None)


def main():
    tests = [
        test_queue_path_matches,
        test_brief_path_matches,
        test_pass_agenda_path_matches,
        test_docs_handoff_path_matches,
        test_memory_handoff_path_matches,
        test_unrelated_path_does_not_match,
        test_empty_path_does_not_match,
        test_check_stale_brief_fires_when_old,
        test_check_stale_brief_does_not_fire_when_recent,
        test_check_stale_brief_fires_once_per_path,
        test_non_read_tool_never_fires,
        test_read_of_non_matching_path_never_fires,
        test_malformed_input_never_raises,
        test_moves_md_has_stale_brief_entry,
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
