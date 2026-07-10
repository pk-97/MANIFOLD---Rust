#!/usr/bin/env python3
"""
Standalone test for the observer half of the Stop-wait CONVERT fix (DESIGN.md
§2 / PASS2_AGENDA item 1): observer.py's poke read/clear helpers and the
_drain priority path (turn-final window classified FIRST when a Stop is
waiting). Never spawns a real classifier — _handle_window is stubbed to record
dispatch order — and never writes real telemetry (VERDICTS_DIR/TELEMETRY_PATH
redirected to a temp dir), same leak-prevention discipline as the sibling
daemon tests.

Run: python3 .claude/daemon/test_stop_poke.py
"""
import importlib.util
import io
import json
import os
import sys
import tempfile
from datetime import datetime, timezone
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
    orig = (observer.VERDICTS_DIR, observer.WORKER_NUDGES_FLAG, valve.VERDICTS_DIR,
            valve.WORKER_NUDGES_FLAG, valve.TELEMETRY_PATH)
    with tempfile.TemporaryDirectory() as td:
        observer.VERDICTS_DIR = td
        observer.WORKER_NUDGES_FLAG = os.path.join(td, "worker-nudges.enabled")
        valve.VERDICTS_DIR = td
        valve.WORKER_NUDGES_FLAG = os.path.join(td, "worker-nudges.enabled")
        valve.TELEMETRY_PATH = os.path.join(td, "telemetry.jsonl")
        valve.append_telemetry = lambda rec: None
        try:
            fn(td)
        finally:
            (observer.VERDICTS_DIR, observer.WORKER_NUDGES_FLAG, valve.VERDICTS_DIR,
             valve.WORKER_NUDGES_FLAG, valve.TELEMETRY_PATH) = orig


def make_daemon(td, transcript="/dev/null"):
    d = observer.Daemon("test-session", transcript)
    return d


def _iso(ts):
    return datetime.fromtimestamp(ts, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def write_two_window_transcript(path):
    """A turn that closes TWO classifier windows: an investigation text
    (end_event_count 1) then a turn-final claim text (end_event_count 2)."""
    lines = [
        {"type": "user", "timestamp": _iso(100), "message": {"role": "user",
         "content": "please fix the overlap bug in the scheduler"}},
        {"type": "assistant", "timestamp": _iso(101), "message": {"role": "assistant", "model": "claude-sonnet-5",
         "content": [{"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "a.rs"}}]}},
        {"type": "user", "timestamp": _iso(102), "message": {"role": "user",
         "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]}},
        {"type": "assistant", "timestamp": _iso(103), "message": {"role": "assistant", "model": "claude-sonnet-5",
         "content": [{"type": "text", "text": "let me investigate the scheduler layout first"}]}},
        {"type": "assistant", "timestamp": _iso(104), "message": {"role": "assistant", "model": "claude-sonnet-5",
         "content": [{"type": "tool_use", "id": "t2", "name": "Read", "input": {"file_path": "b.rs"}}]}},
        {"type": "user", "timestamp": _iso(105), "message": {"role": "user",
         "content": [{"type": "tool_result", "tool_use_id": "t2", "content": "ok"}]}},
        {"type": "assistant", "timestamp": _iso(106), "message": {"role": "assistant", "model": "claude-sonnet-5",
         "content": [{"type": "text", "text": "the fix is in and the tests pass now"}]}},
    ]
    with open(path, "w", encoding="utf-8") as f:
        f.write("\n".join(json.dumps(x) for x in lines) + "\n")


# ---- _read_poke / _maybe_clear_poke ----


def test_read_poke_missing_is_none():
    def run(td):
        d = make_daemon(td)
        check("no poke file -> None", d._read_poke() is None)
    with_temp_dirs(run)


def test_read_poke_reads_int():
    def run(td):
        d = make_daemon(td)
        with open(d.poke_path, "w", encoding="utf-8") as f:
            f.write("4210")
        check("poke value read as int", d._read_poke() == 4210, d._read_poke())
    with_temp_dirs(run)


def test_read_poke_junk_is_none():
    def run(td):
        d = make_daemon(td)
        with open(d.poke_path, "w", encoding="utf-8") as f:
            f.write("not-a-number")
        check("malformed poke -> None (fail open)", d._read_poke() is None)
    with_temp_dirs(run)


def test_maybe_clear_poke_clears_when_target_reached():
    def run(td):
        d = make_daemon(td)
        with open(d.poke_path, "w", encoding="utf-8") as f:
            f.write("100")
        d._maybe_clear_poke(poke=100, offset=100, size=200, logf=io.StringIO())
        check("poke cleared once offset reaches target", not os.path.exists(d.poke_path))
    with_temp_dirs(run)


def test_maybe_clear_poke_clears_when_fully_drained_stale_target():
    def run(td):
        d = make_daemon(td)
        with open(d.poke_path, "w", encoding="utf-8") as f:
            f.write("999")
        # offset >= size: everything on disk is drained; target unreachable ->
        # clear anyway so the Stop hook isn't pinned forever on a stale target.
        d._maybe_clear_poke(poke=999, offset=50, size=50, logf=io.StringIO())
        check("poke cleared when fully caught up to disk", not os.path.exists(d.poke_path))
    with_temp_dirs(run)


def test_maybe_clear_poke_keeps_when_behind():
    def run(td):
        d = make_daemon(td)
        with open(d.poke_path, "w", encoding="utf-8") as f:
            f.write("100")
        d._maybe_clear_poke(poke=100, offset=50, size=200, logf=io.StringIO())
        check("poke kept while still behind target", os.path.exists(d.poke_path))
    with_temp_dirs(run)


def test_maybe_clear_poke_noop_when_no_poke():
    def run(td):
        d = make_daemon(td)
        # No poke file, poke=None: must not raise and must not create anything.
        d._maybe_clear_poke(poke=None, offset=10, size=10, logf=io.StringIO())
        check("no-poke path is a safe no-op", not os.path.exists(d.poke_path))
    with_temp_dirs(run)


# ---- _drain priority ordering ----


def test_priority_drain_classifies_newest_window_first():
    def run(td):
        transcript = os.path.join(td, "t.jsonl")
        write_two_window_transcript(transcript)
        d = make_daemon(td, transcript)
        order = []
        d._handle_window = lambda w, logf, **kw: order.append(w["end_event_count"])
        d._drain(0, io.StringIO(), classify=True, priority=True)
        check("priority mode classifies turn-final (newest) window first", order == [2, 1], order)
    with_temp_dirs(run)


def test_fifo_drain_classifies_in_order():
    def run(td):
        transcript = os.path.join(td, "t.jsonl")
        write_two_window_transcript(transcript)
        d = make_daemon(td, transcript)
        order = []
        d._handle_window = lambda w, logf, **kw: order.append(w["end_event_count"])
        d._drain(0, io.StringIO(), classify=True, priority=False)
        check("FIFO (normal) mode classifies oldest window first", order == [1, 2], order)
    with_temp_dirs(run)


def test_catchup_drain_never_classifies():
    def run(td):
        transcript = os.path.join(td, "t.jsonl")
        write_two_window_transcript(transcript)
        d = make_daemon(td, transcript)
        order = []
        d._handle_window = lambda w, logf, **kw: order.append(w["end_event_count"])
        d._drain(0, io.StringIO(), classify=False, priority=False)
        check("catchup (classify=False) classifies nothing", order == [], order)
    with_temp_dirs(run)


def test_poke_honored_end_to_end_pieces():
    # The loop body's poke path, exercised as its pieces: read the poke, drain
    # in priority mode, clear the poke once caught up.
    def run(td):
        transcript = os.path.join(td, "t.jsonl")
        write_two_window_transcript(transcript)
        size = os.path.getsize(transcript)
        d = make_daemon(td, transcript)
        with open(d.poke_path, "w", encoding="utf-8") as f:
            f.write(str(size))
        order = []
        d._handle_window = lambda w, logf, **kw: order.append(w["end_event_count"])
        poke = d._read_poke()
        check("poke read as the transcript size", poke == size, (poke, size))
        offset = d._drain(0, io.StringIO(), classify=True, priority=poke is not None)
        d._maybe_clear_poke(poke, offset, size, io.StringIO())
        check("turn-final window classified first under the poke", order and order[0] == 2, order)
        check("poke cleared after classifying through the target", not os.path.exists(d.poke_path))
    with_temp_dirs(run)


def main():
    test_read_poke_missing_is_none()
    test_read_poke_reads_int()
    test_read_poke_junk_is_none()
    test_maybe_clear_poke_clears_when_target_reached()
    test_maybe_clear_poke_clears_when_fully_drained_stale_target()
    test_maybe_clear_poke_keeps_when_behind()
    test_maybe_clear_poke_noop_when_no_poke()
    test_priority_drain_classifies_newest_window_first()
    test_fifo_drain_classifies_in_order()
    test_catchup_drain_never_classifies()
    test_poke_honored_end_to_end_pieces()

    for name in PASS:
        print(f"PASS: {name}")
    for name, detail in FAIL:
        print(f"FAIL: {name} ({detail!r})")
    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
