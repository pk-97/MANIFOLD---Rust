#!/usr/bin/env python3
"""
Standalone test for DESIGN.md §2h.5's hygiene sweep: observer.py's
Daemon._hygiene_sweep, run once at observer startup to remove stale
verdicts/ sentinels — `.stopblock.*` older than 7 days, and orphan
`.stop` files (no live pidfile for their session) older than 7 days.
Conservative by construction: age-gated, and every other file/dir
(.pid, .json verdicts, .consumed, .firestate.json, .offset, mutes/) is
left alone because the sweep's patterns never match them.

Run: python3 .claude/daemon/test_hygiene_sweep.py
"""
import importlib.util
import io
import os
import sys
import tempfile
import time
from pathlib import Path

DAEMON_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(DAEMON_DIR))

spec = importlib.util.spec_from_file_location("observer", DAEMON_DIR / "observer.py")
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


OLD = 8 * 86400  # older than HYGIENE_MAX_AGE_S (7 days)
YOUNG = 60  # one minute — well inside the retention window


def touch(path, age_seconds):
    """Create an empty file and backdate its mtime by `age_seconds`."""
    with open(path, "w", encoding="utf-8") as f:
        f.write("x")
    ts = time.time() - age_seconds
    os.utime(path, (ts, ts))


def with_temp_verdicts(fn):
    orig = observer.VERDICTS_DIR
    with tempfile.TemporaryDirectory() as td:
        observer.VERDICTS_DIR = td
        try:
            fn(td)
        finally:
            observer.VERDICTS_DIR = orig


# ---- .stopblock.* ----


def test_old_stopblock_removed():
    def run(td):
        path = os.path.join(td, "sess1.stopblock.prompt123")
        touch(path, OLD)
        observer.Daemon._hygiene_sweep(io.StringIO())
        check("old stopblock sentinel removed", not os.path.exists(path))
    with_temp_verdicts(run)


def test_young_stopblock_kept():
    def run(td):
        path = os.path.join(td, "sess1.stopblock.prompt123")
        touch(path, YOUNG)
        observer.Daemon._hygiene_sweep(io.StringIO())
        check("young stopblock sentinel kept (< 7 days)", os.path.exists(path))
    with_temp_verdicts(run)


# ---- orphan .stop ----


def test_old_orphan_stop_removed():
    def run(td):
        path = os.path.join(td, "deadsess.stop")
        touch(path, OLD)
        # No deadsess.pid at all -> orphan by construction.
        observer.Daemon._hygiene_sweep(io.StringIO())
        check("old orphan .stop removed", not os.path.exists(path))
    with_temp_verdicts(run)


def test_old_stop_with_dead_pid_removed():
    def run(td):
        path = os.path.join(td, "deadsess.stop")
        touch(path, OLD)
        # A pidfile naming a PID that certainly doesn't exist.
        with open(os.path.join(td, "deadsess.pid"), "w", encoding="utf-8") as f:
            f.write("999999999")
        observer.Daemon._hygiene_sweep(io.StringIO())
        check("old .stop with a dead pidfile is still an orphan -> removed", not os.path.exists(path))
    with_temp_verdicts(run)


def test_old_stop_with_live_pid_kept():
    def run(td):
        path = os.path.join(td, "livesess.stop")
        touch(path, OLD)
        # Our own PID is guaranteed alive.
        with open(os.path.join(td, "livesess.pid"), "w", encoding="utf-8") as f:
            f.write(str(os.getpid()))
        observer.Daemon._hygiene_sweep(io.StringIO())
        check("old .stop for a still-live session is NOT an orphan -> kept", os.path.exists(path))
    with_temp_verdicts(run)


def test_young_orphan_stop_kept():
    def run(td):
        path = os.path.join(td, "deadsess.stop")
        touch(path, YOUNG)
        observer.Daemon._hygiene_sweep(io.StringIO())
        check("young orphan .stop kept (< 7 days) even though orphaned", os.path.exists(path))
    with_temp_verdicts(run)


# ---- conservative: everything else is untouched regardless of age ----


def test_old_pidfile_never_touched():
    def run(td):
        path = os.path.join(td, "sess1.pid")
        touch(path, OLD)
        observer.Daemon._hygiene_sweep(io.StringIO())
        check(".pid file never removed by hygiene sweep", os.path.exists(path))
    with_temp_verdicts(run)


def test_old_verdict_json_never_touched():
    def run(td):
        path = os.path.join(td, "sess1.json")
        touch(path, OLD)
        observer.Daemon._hygiene_sweep(io.StringIO())
        check(".json verdict file never removed", os.path.exists(path))
    with_temp_verdicts(run)


def test_old_consumed_marker_never_touched():
    def run(td):
        path = os.path.join(td, "sess1.consumed")
        touch(path, OLD)
        observer.Daemon._hygiene_sweep(io.StringIO())
        check(".consumed marker never removed", os.path.exists(path))
    with_temp_verdicts(run)


def test_old_firestate_never_touched():
    def run(td):
        path = os.path.join(td, "sess1.firestate.json")
        touch(path, OLD)
        observer.Daemon._hygiene_sweep(io.StringIO())
        check(".firestate.json never removed", os.path.exists(path))
    with_temp_verdicts(run)


def test_old_offset_never_touched():
    def run(td):
        path = os.path.join(td, "sess1.offset")
        touch(path, OLD)
        observer.Daemon._hygiene_sweep(io.StringIO())
        check(".offset heartbeat never removed", os.path.exists(path))
    with_temp_verdicts(run)


def test_old_log_never_touched():
    def run(td):
        path = os.path.join(td, "sess1.log")
        touch(path, OLD)
        observer.Daemon._hygiene_sweep(io.StringIO())
        check(".log file never removed (not a targeted pattern)", os.path.exists(path))
    with_temp_verdicts(run)


def test_mutes_dir_never_descended_into():
    def run(td):
        mutes_dir = os.path.join(td, "mutes")
        os.makedirs(mutes_dir)
        mute_path = os.path.join(mutes_dir, "anchor__verify-claim.json")
        touch(mute_path, OLD)
        observer.Daemon._hygiene_sweep(io.StringIO())
        check("mutes/ directory itself untouched", os.path.isdir(mutes_dir))
        check("a mute file, even if it happened to look old, is never scanned (not top-level)", os.path.exists(mute_path))
    with_temp_verdicts(run)


# ---- fail-open + logging ----


def test_sweep_never_raises_when_verdicts_dir_missing():
    orig = observer.VERDICTS_DIR
    observer.VERDICTS_DIR = "/nonexistent/path/for/hygiene/sweep/test"
    try:
        logf = io.StringIO()
        observer.Daemon._hygiene_sweep(logf)  # must not raise
        check("sweep against a missing VERDICTS_DIR does not raise", True)
    except Exception as e:  # pragma: no cover - only hit on a real regression
        check("sweep against a missing VERDICTS_DIR does not raise", False, e)
    finally:
        observer.VERDICTS_DIR = orig


def test_sweep_logs_what_it_removed():
    def run(td):
        path = os.path.join(td, "sess1.stopblock.abc")
        touch(path, OLD)
        logf = io.StringIO()
        observer.Daemon._hygiene_sweep(logf)
        text = logf.getvalue()
        check("sweep logs the removed sentinel name", "sess1.stopblock.abc" in text, text)
    with_temp_verdicts(run)


def test_sweep_logs_nothing_to_remove():
    def run(td):
        logf = io.StringIO()
        observer.Daemon._hygiene_sweep(logf)
        text = logf.getvalue()
        check("sweep logs when there was nothing to remove", "nothing to remove" in text, text)
    with_temp_verdicts(run)


def test_sweep_runs_once_at_run_startup():
    """DESIGN.md §2h.5: "runs once per observer start." Confirms the call
    site is in _run (before the poll loop) by grepping the source rather
    than driving the full loop (which would need a live transcript file and
    a real classifier) — a structural check, not a behavioral one, but the
    behavioral piece (the sweep itself firing correctly) is covered above."""
    src = (DAEMON_DIR / "observer.py").read_text(encoding="utf-8")
    run_body = src.split("def _run(self, logf):", 1)[1].split("\n    def ", 1)[0]
    check("_hygiene_sweep is called from _run", "_hygiene_sweep(logf)" in run_body, run_body[:400])
    while_idx = run_body.find("while True:")
    call_idx = run_body.find("_hygiene_sweep(logf)")
    check("the call site precedes the poll loop", 0 <= call_idx < while_idx, (call_idx, while_idx))


def main():
    tests = [
        test_old_stopblock_removed,
        test_young_stopblock_kept,
        test_old_orphan_stop_removed,
        test_old_stop_with_dead_pid_removed,
        test_old_stop_with_live_pid_kept,
        test_young_orphan_stop_kept,
        test_old_pidfile_never_touched,
        test_old_verdict_json_never_touched,
        test_old_consumed_marker_never_touched,
        test_old_firestate_never_touched,
        test_old_offset_never_touched,
        test_old_log_never_touched,
        test_mutes_dir_never_descended_into,
        test_sweep_never_raises_when_verdicts_dir_missing,
        test_sweep_logs_what_it_removed,
        test_sweep_logs_nothing_to_remove,
        test_sweep_runs_once_at_run_startup,
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
