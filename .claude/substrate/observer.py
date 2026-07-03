#!/usr/bin/env python3
"""Substrate observer daemon — DESIGN.md §1.

Tails one session's transcript, closes windows with common.WindowState (the
same windowing replay.py uses — they must never drift apart), classifies
each closed window on Haiku, and writes a verdict file the valve hooks read.
Spawned detached by the SessionStart hook, one per session_id (guarded by a
pidfile). Exits when the transcript goes idle > IDLE_TIMEOUT_S or a .stop
sentinel appears (written by the SessionEnd hook — see that hook's
docstring for why it isn't Stop).

Fail-open is load-bearing here (DESIGN.md invariant 1): every step that can
fail — a classifier timeout, a torn transcript line, a stale pidfile — must
leave the session alone and, at worst, degrade to silence. Nothing in this
file may propagate an exception into the coding session; the whole run
loop is wrapped and any escape is logged to <session>.log, not raised.
"""
import argparse
import json
import os
import signal
import sys
import time
import traceback

SUBSTRATE_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, SUBSTRATE_DIR)
import common  # noqa: E402

VERDICTS_DIR = os.path.join(SUBSTRATE_DIR, "verdicts")
MOVES_PATH = os.path.join(SUBSTRATE_DIR, "moves.md")
RUBRIC_PATH = os.path.join(SUBSTRATE_DIR, "rubric.md")

POLL_SECONDS = 3
IDLE_TIMEOUT_S = 600  # DESIGN.md §1: idle > 10 min ends the daemon
ESCALATE_AFTER = 2  # "flagged again after two injections" -> the 3rd fire escalates


def _log(logf, msg):
    try:
        logf.write(f"{time.strftime('%H:%M:%S')} {msg}\n")
        logf.flush()
    except Exception:
        pass


def _atomic_write_json(path, obj):
    tmp = f"{path}.tmp.{os.getpid()}"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(obj, f)
    os.replace(tmp, path)


class Daemon:
    def __init__(self, session_id, transcript_path):
        self.session_id = session_id
        self.transcript_path = transcript_path
        self.verdict_path = os.path.join(VERDICTS_DIR, f"{session_id}.json")
        self.pid_path = os.path.join(VERDICTS_DIR, f"{session_id}.pid")
        self.stop_path = os.path.join(VERDICTS_DIR, f"{session_id}.stop")
        self.consumed_path = os.path.join(VERDICTS_DIR, f"{session_id}.consumed")
        self.log_path = os.path.join(VERDICTS_DIR, f"{session_id}.log")

        self.moves = common.parse_moves(common.read(MOVES_PATH))
        self.system_prompt = common.build_system_prompt(common.read(RUBRIC_PATH), self.moves)
        self.state = common.WindowState()

        self.last_fire_event = {}  # move_id -> event_count of its last delivered fire
        self.fire_count = {}  # move_id -> times delivered (post-cooldown) this session
        self.escalated = False  # escalate/checkpoint fires at most once per session
        self.next_seq = 1  # re-seeded in _run from the persistent consumed marker
        self.phase = "orienting"
        self.owns_pidfile = False

    # ---- lifecycle ----

    def _already_running(self):
        try:
            with open(self.pid_path, encoding="utf-8") as f:
                pid = int(f.read().strip())
            os.kill(pid, 0)  # signal 0: existence check only
            return True
        except (OSError, ValueError):
            return False

    def _claim_pidfile(self):
        with open(self.pid_path, "w", encoding="utf-8") as f:
            f.write(str(os.getpid()))
        self.owns_pidfile = True

    def _cleanup(self):
        # A duplicate spawn that lost the pidfile race must not delete the
        # live daemon's pidfile on its way out — that orphans the live daemon
        # (SessionEnd can't find it) and lets a later spawn create a second
        # concurrent tailer with colliding seq numbers.
        if not self.owns_pidfile:
            return
        for p in (self.pid_path, self.stop_path):
            try:
                os.remove(p)
            except OSError:
                pass

    def run(self):
        os.makedirs(VERDICTS_DIR, exist_ok=True)
        # SessionEnd stops us with SIGTERM; Python's default handler skips
        # `finally`, which would strand the pidfile and .stop sentinel — and a
        # stale .stop ends the *next* daemon for this session on its first
        # poll. Convert to SystemExit so cleanup below always runs.
        signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
        with open(self.log_path, "a", encoding="utf-8") as logf:
            try:
                self._run(logf)
            except Exception:
                _log(logf, "FATAL:\n" + traceback.format_exc())
            finally:
                self._cleanup()

    def _run(self, logf):
        if self._already_running():
            _log(logf, "another daemon already running for this session, exiting")
            return
        self._claim_pidfile()
        # Anything in the mailbox from before our spawn is a predecessor's:
        # a leftover .stop (SIGKILLed daemon, pre-fix SIGTERM) would end us on
        # the first poll, and the consumed marker persists across restarts —
        # if we restarted seq at 1, every new flag would read as already
        # consumed and silently never deliver.
        try:
            os.remove(self.stop_path)
        except OSError:
            pass
        prior = self._read_verdict_file() or {}
        prior_seq = (prior.get("flag") or {}).get("seq") or 0
        self.next_seq = max(self._read_consumed(), prior_seq) + 1
        _log(logf, f"observer started, pid={os.getpid()}, next_seq={self.next_seq}, transcript={self.transcript_path}")

        offset = self._catchup(logf)
        last_activity = time.time()

        while True:
            if os.path.exists(self.stop_path):
                _log(logf, "stop sentinel seen, exiting")
                break
            try:
                size = os.path.getsize(self.transcript_path)
            except OSError:
                size = offset
            if size > offset:
                offset = self._drain(offset, logf, classify=True)
                last_activity = time.time()
            elif time.time() - last_activity > IDLE_TIMEOUT_S:
                _log(logf, "idle timeout, exiting")
                break
            time.sleep(POLL_SECONDS)

    # ---- transcript reading ----

    def _catchup(self, logf):
        """Replay everything already on disk to rebuild window state (task,
        recent texts, ledger) without spending classifier calls on history —
        only live-tailed windows get classified. Matters for `resume`/
        `compact` sessions, which start with real history already present."""
        if not os.path.exists(self.transcript_path):
            return 0
        offset = self._drain(0, logf, classify=False)
        _log(logf, f"catchup done, offset={offset}, task={self.state.current_task!r}")
        return offset

    def _drain(self, offset, logf, classify):
        """Read whole lines from `offset` to EOF, feed each into WindowState,
        classify any window it closes (if `classify`), and return the new
        offset. A trailing partial line (mid-write) is left for next poll."""
        # errors="replace": a torn multi-byte char at EOF (writer mid-write)
        # would otherwise raise UnicodeDecodeError and kill the daemon for
        # the rest of the session; the torn line has no trailing \n, so it is
        # re-read intact on the next poll either way.
        with open(self.transcript_path, encoding="utf-8", errors="replace") as f:
            f.seek(offset)
            while True:
                # readline(), not `for line in f` — iteration protocol uses
                # internal read-ahead buffering that makes f.tell() raise
                # ("telling position disabled by next() call").
                line = f.readline()
                if not line or not line.endswith("\n"):
                    break
                try:
                    self._feed_line(line, classify, logf)
                except Exception:
                    # one malformed transcript line must cost one line, not
                    # the whole week of observation
                    _log(logf, "feed_line error:\n" + traceback.format_exc())
                offset = f.tell()
        return offset

    def _feed_line(self, line, classify, logf):
        line = line.strip()
        if not line:
            return
        try:
            d = json.loads(line)
        except json.JSONDecodeError:
            return
        etype = d.get("type")
        if etype not in ("user", "assistant"):
            return
        content = d.get("message", {}).get("content")
        ts = common.parse_ts(d.get("timestamp"))
        closed = None
        if etype == "assistant":
            if isinstance(content, list) and content:
                closed = self.state.feed_assistant_content(content, ts)
        else:
            if content is not None and not (isinstance(content, list) and not content):
                closed, _human = self.state.feed_user_content(content, ts)
        if closed and classify:
            self._handle_window(closed, logf)

    # ---- classification + verdict mailbox ----

    def _handle_window(self, window, logf):
        verdict = common.call_classifier(self.system_prompt, window["text"])
        if "error" in verdict:
            _log(logf, f"classifier error: {verdict['error']}")
            return  # fail open — leave the verdict file as it was

        self.phase = verdict.get("phase") or self.phase
        raw_flag = verdict.get("flag")
        move_id = common.validate_move_id(raw_flag, self.moves)
        if raw_flag and not move_id:
            _log(logf, f"rejected unknown/invalid move id from classifier: {raw_flag!r}")

        flag_out = self._resolve_fire(window["end_event_count"], move_id, verdict, logf) if move_id else None

        record = {
            "ts": time.time(),
            "window_range": {"end_event_count": window["end_event_count"], "end_ts": window["end_ts"]},
            "phase": self.phase,
        }
        if flag_out:
            record["flag"] = flag_out
        else:
            # Never clobber an undelivered whisper with null — DESIGN.md
            # invariant 3 is "one whisper at a time", not "zero eventually".
            prior = self._read_verdict_file()
            record["flag"] = prior["flag"] if prior and prior.get("flag") else None
        _atomic_write_json(self.verdict_path, record)

    def _read_verdict_file(self):
        try:
            with open(self.verdict_path, encoding="utf-8") as f:
                return json.load(f)
        except (OSError, json.JSONDecodeError):
            return None

    def _read_consumed(self):
        try:
            with open(self.consumed_path, encoding="utf-8") as f:
                return int(f.read().strip() or "0")
        except (OSError, ValueError):
            return 0

    def _resolve_fire(self, event_count, move_id, verdict, logf):
        cd_class = self.moves.get(move_id, {}).get("cooldown", "standard")
        if cd_class == "once":
            if self.fire_count.get(move_id, 0) >= 1:
                return None
        else:
            limit = common.COOLDOWN_EVENTS.get(cd_class, 20)
            prev = self.last_fire_event.get(move_id)
            if prev is not None and (event_count - prev) < limit:
                return None

        # One live flag at a time: don't raise a new one while the last
        # delivered flag is still sitting unconsumed in the mailbox.
        prior = self._read_verdict_file()
        if prior and prior.get("flag"):
            prior_seq = prior["flag"].get("seq")
            if prior_seq is not None and self._read_consumed() < prior_seq:
                _log(logf, f"suppressed {move_id} — prior flag seq={prior_seq} still undelivered")
                return None

        self.last_fire_event[move_id] = event_count
        self.fire_count[move_id] = self.fire_count.get(move_id, 0) + 1
        effective_id = move_id

        if self.fire_count[move_id] > ESCALATE_AFTER and not self.escalated and "escalate/checkpoint" in self.moves:
            effective_id = "escalate/checkpoint"
            self.escalated = True
            _log(logf, f"escalating {move_id} -> escalate/checkpoint after {self.fire_count[move_id]} fires")

        seq = self.next_seq
        self.next_seq += 1
        return {
            "move_id": effective_id,
            "evidence": verdict.get("evidence"),
            "confidence": verdict.get("confidence"),
            "seq": seq,
        }


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--session-id", required=True)
    ap.add_argument("--transcript", required=True)
    args = ap.parse_args()
    Daemon(args.session_id, args.transcript).run()


if __name__ == "__main__":
    main()
