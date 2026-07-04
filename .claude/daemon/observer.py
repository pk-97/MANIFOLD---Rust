#!/usr/bin/env python3
"""Daemon observer — DESIGN.md §1.

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
import collections
import json
import os
import signal
import sys
import time
import traceback

DAEMON_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, DAEMON_DIR)
import common  # noqa: E402
import valve  # noqa: E402

VERDICTS_DIR = os.path.join(DAEMON_DIR, "verdicts")
MOVES_PATH = os.path.join(DAEMON_DIR, "moves.md")
RUBRIC_PATH = os.path.join(DAEMON_DIR, "rubric.md")
MUTES_DIR = os.path.join(VERDICTS_DIR, "mutes")

POLL_SECONDS = 3
IDLE_TIMEOUT_S = 600  # DESIGN.md §1: idle > 10 min ends the daemon
ESCALATE_AFTER = 2  # "flagged again after two injections" -> the 3rd fire escalates

# DESIGN.md §4b: outcome scoring. Only these three families have a defined
# mechanical signal; every other move_id (coaching moves, escalate/checkpoint)
# scores "unscored" the moment its delivery is observed — never guessed.
SCORE_WINDOW_EVENTS = 10
VERIFY_CLAIM_SIGNALS = (
    "cargo test", "cargo run", "cargo build", "cargo bench", "cargo clippy",
    "pytest", "npm test", "npm run", "go test", "swift test",
    "render", "screenshot", ".png", "headless",
)
MUTE_FIRE_THRESHOLD = 5  # >=5 scored fires, 0 scored successes -> auto-mute
MUTE_DURATION_S = 7 * 86400


def _move_family(move_id):
    if move_id == "anchor/verify-claim":
        return "verify-claim"
    if move_id == "anchor/thrash":
        return "thrash"
    if move_id == "anchor/circling":
        return "circling"
    return None


def _tool_class(tool_label_str):
    """Strip the `[type@model]` suffix `common.tool_label` adds for Agent/Task
    calls, leaving the bare tool class ("Agent", "Bash", "Read", ...)."""
    return tool_label_str.split("[", 1)[0]


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


WORKER_NUDGES_FLAG = os.path.join(VERDICTS_DIR, "worker-nudges.enabled")


def _worker_nudges_enabled():
    return os.path.exists(WORKER_NUDGES_FLAG)


class AgentWorker:
    """Per-subagent mailbox + window state (DESIGN.md §2b, shipped OFF behind
    WORKER_NUDGES_FLAG). Deliberately duck-types the subset of Daemon's own
    attributes that `_handle_window`/`_resolve_fire`/`_read_verdict_file`/
    `_read_consumed` read through `mailbox`, so those methods run unmodified
    for agents instead of a forked copy that could drift from the session
    path. No §4b scoring here — that stays session-only (see `_resolve_fire`)."""

    def __init__(self, agent_id, session_id, transcript_path):
        self.agent_id = agent_id
        key = f"{session_id}.{agent_id}"
        self.verdict_path = os.path.join(VERDICTS_DIR, f"{key}.json")
        self.consumed_path = os.path.join(VERDICTS_DIR, f"{key}.consumed")
        self.transcript_path = transcript_path
        self.offset = 0
        self.state = common.WindowState()
        self.last_fire_event = {}
        self.fire_count = {}
        self.escalated = False
        self.next_seq = 1
        self.phase = "orienting"


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

        # DESIGN.md §4b: outcome scoring + auto-mute.
        self.fire_records = {}  # seq -> move_id, for every flag *this instance* raised
        self.pending_scores = {}  # seq -> {move_id, start_event_count, baseline_tool_class}
        self.recent_events = collections.deque(maxlen=64)  # (event_count, tool_class, status)
        self.last_consumed_seq = 0  # re-seeded in _run; tracks delivery via the consumed marker

        # DESIGN.md §2b: worker nudges, shipped OFF (see _worker_nudges_enabled).
        self.session_dir = os.path.dirname(transcript_path)
        self.agents = {}  # agent_id -> AgentWorker

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
        # Seed from disk, not 0: fires from a PRIOR instance of this observer
        # (before an idle-exit revive) are already consumed and must not be
        # mistaken for new deliveries — fire_records starts empty each
        # instance anyway, so this is a perf guard, not a correctness one.
        self.last_consumed_seq = self._read_consumed()
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
            self._check_deliveries(logf)
            self._score_pending(logf)
            self._scan_agents(logf)
            time.sleep(POLL_SECONDS)

    # ---- worker nudges (DESIGN.md §2b, shipped OFF) ----

    def _scan_agents(self, logf):
        """No-op unless WORKER_NUDGES_FLAG exists — the whole subagent-tailing
        feature is otherwise unreachable code, per the ship-dark requirement."""
        if not _worker_nudges_enabled():
            return
        subdir = os.path.join(self.session_dir, "subagents")
        try:
            names = os.listdir(subdir)
        except OSError:
            return
        for name in sorted(names):
            if not (name.startswith("agent-") and name.endswith(".jsonl")):
                continue
            agent_id = name[len("agent-") : -len(".jsonl")]
            if agent_id in self.agents:
                continue
            worker = AgentWorker(agent_id, self.session_id, os.path.join(subdir, name))
            self.agents[agent_id] = worker
            try:
                worker.offset = self._agent_drain(worker, logf, classify=False)
                _log(logf, f"worker-nudges: discovered agent {agent_id}, catchup offset={worker.offset}")
            except Exception:
                _log(logf, f"worker-nudges: catchup failed for {agent_id}:\n" + traceback.format_exc())
        for agent_id, worker in list(self.agents.items()):
            try:
                size = os.path.getsize(worker.transcript_path)
            except OSError:
                continue
            if size > worker.offset:
                try:
                    worker.offset = self._agent_drain(worker, logf, classify=True)
                except Exception:
                    _log(logf, f"worker-nudges: drain failed for {agent_id}:\n" + traceback.format_exc())

    def _agent_drain(self, worker, logf, classify):
        """Mirrors `_drain`, reading `worker.transcript_path` into
        `worker.state` instead of `self.state`. No outcome-scoring ledger —
        that stays session-only (§4b is out of this item's scope)."""
        with open(worker.transcript_path, encoding="utf-8", errors="replace") as f:
            f.seek(worker.offset)
            offset = worker.offset
            while True:
                line = f.readline()
                if not line or not line.endswith("\n"):
                    break
                try:
                    self._agent_feed_line(worker, line, classify, logf)
                except Exception:
                    _log(logf, f"worker-nudges: feed_line error ({worker.agent_id}):\n" + traceback.format_exc())
                offset = f.tell()
        return offset

    def _agent_feed_line(self, worker, line, classify, logf):
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
                closed = worker.state.feed_assistant_content(content, ts, model=d.get("message", {}).get("model"))
        else:
            if content is not None and not (isinstance(content, list) and not content):
                closed, _human = worker.state.feed_user_content(content, ts)
        if closed and classify:
            self._handle_window(closed, logf, mailbox=worker)

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
                closed = self.state.feed_assistant_content(content, ts, model=d.get("message", {}).get("model"))
        else:
            if content is not None and not (isinstance(content, list) and not content):
                # Peek tool_result -> (name, input) BEFORE feed_user_content
                # pops them from state.pending, so the outcome-scoring ledger
                # (§4b) can see what ran without duplicating WindowState's
                # own bookkeeping.
                count_before = self.state.total_tool_event_count
                peeked = self._peek_tool_events(content)
                closed, _human = self.state.feed_user_content(content, ts)
                for idx, (name, input_, status) in enumerate(peeked):
                    label = common.tool_label(name, input_, self.state.session_model)
                    target = common.tool_target(input_)
                    self.recent_events.append((count_before + idx + 1, _tool_class(label), status, target))
        if closed and classify:
            self._handle_window(closed, logf)

    def _peek_tool_events(self, content):
        """Read-only pass over tool_result blocks -> [(name, input, status)],
        in the same order common.WindowState.feed_user_content will consume
        them. Must run BEFORE that call, which pops state.pending."""
        events = []
        if not isinstance(content, list):
            return events
        for c in content:
            if isinstance(c, dict) and c.get("type") == "tool_result":
                name, input_ = self.state.pending.get(c.get("tool_use_id"), ("?", {}))
                events.append((name, input_, common.tool_result_status(c)))
        return events

    # ---- classification + verdict mailbox ----

    def _handle_window(self, window, logf, mailbox=None):
        """`mailbox=None` is the main session (uses `self.*` directly — the
        exact, unchanged path). DESIGN.md §2b's worker-nudges extension
        passes an `AgentWorker` here instead, which duck-types the same
        `verdict_path`/`phase`/fire-tracking attributes `self` has, so this
        method (and `_resolve_fire` below) is reused verbatim rather than
        forked — the two paths cannot drift apart the way separately
        maintained copies would."""
        mb = mailbox if mailbox is not None else self
        verdict = common.call_classifier(self.system_prompt, window["text"])
        if "error" in verdict:
            _log(logf, f"classifier error: {verdict['error']}")
            return  # fail open — leave the verdict file as it was

        mb.phase = verdict.get("phase") or mb.phase
        raw_flag = verdict.get("flag")
        move_id = common.validate_move_id(raw_flag, self.moves)
        if raw_flag and not move_id:
            _log(logf, f"rejected unknown/invalid move id from classifier: {raw_flag!r}")

        flag_out = self._resolve_fire(window["end_event_count"], move_id, verdict, logf, mailbox=mailbox) if move_id else None

        record = {
            "ts": time.time(),
            "window_range": {"end_event_count": window["end_event_count"], "end_ts": window["end_ts"]},
            "phase": mb.phase,
        }
        if flag_out:
            record["flag"] = flag_out
        else:
            # Never clobber an undelivered whisper with null — DESIGN.md
            # invariant 3 is "one whisper at a time", not "zero eventually".
            prior = self._read_verdict_file(mailbox)
            record["flag"] = prior["flag"] if prior and prior.get("flag") else None
        _atomic_write_json(mb.verdict_path, record)

    def _read_verdict_file(self, mailbox=None):
        mb = mailbox if mailbox is not None else self
        try:
            with open(mb.verdict_path, encoding="utf-8") as f:
                return json.load(f)
        except (OSError, json.JSONDecodeError):
            return None

    def _read_consumed(self, mailbox=None):
        mb = mailbox if mailbox is not None else self
        try:
            with open(mb.consumed_path, encoding="utf-8") as f:
                return int(f.read().strip() or "0")
        except (OSError, ValueError):
            return 0

    def _resolve_fire(self, event_count, move_id, verdict, logf, mailbox=None):
        mb = mailbox if mailbox is not None else self
        if self._is_muted(move_id):
            _log(logf, f"suppressed {move_id} — auto-muted (DESIGN.md §4b)")
            return None
        cd_class = self.moves.get(move_id, {}).get("cooldown", "standard")
        if cd_class == "once":
            if mb.fire_count.get(move_id, 0) >= 1:
                return None
        else:
            limit = common.COOLDOWN_EVENTS.get(cd_class, 20)
            prev = mb.last_fire_event.get(move_id)
            if prev is not None and (event_count - prev) < limit:
                return None

        # One live flag at a time: don't raise a new one while the last
        # delivered flag is still sitting unconsumed in the mailbox.
        prior = self._read_verdict_file(mailbox)
        if prior and prior.get("flag"):
            prior_seq = prior["flag"].get("seq")
            if prior_seq is not None and self._read_consumed(mailbox) < prior_seq:
                _log(logf, f"suppressed {move_id} — prior flag seq={prior_seq} still undelivered")
                return None

        mb.last_fire_event[move_id] = event_count
        mb.fire_count[move_id] = mb.fire_count.get(move_id, 0) + 1
        effective_id = move_id

        if mb.fire_count[move_id] > ESCALATE_AFTER and not mb.escalated and "escalate/checkpoint" in self.moves:
            effective_id = "escalate/checkpoint"
            mb.escalated = True
            _log(logf, f"escalating {move_id} -> escalate/checkpoint after {mb.fire_count[move_id]} fires")

        seq = mb.next_seq
        mb.next_seq += 1
        if mailbox is None:
            # §4b outcome scoring is session-only for now (item 3's scope is
            # delivery infra, not scoring) — and mailbox.next_seq is a
            # separate counter per agent, so recording it here under the
            # session's shared self.fire_records would collide seq numbers
            # across mailboxes.
            self.fire_records[seq] = effective_id
        return {
            "move_id": effective_id,
            "evidence": verdict.get("evidence"),
            "confidence": verdict.get("confidence"),
            "seq": seq,
        }

    # ---- outcome scoring + auto-mute (DESIGN.md §4b) ----

    def _check_deliveries(self, logf):
        """Poll the consumed-seq marker (written by the valve hooks in a
        different process) for fires this instance raised. A newly-consumed
        seq means that flag was just delivered to the model — start its
        scoring window from the ledger position right now."""
        consumed = self._read_consumed()
        if consumed <= self.last_consumed_seq:
            return
        for seq in range(self.last_consumed_seq + 1, consumed + 1):
            move_id = self.fire_records.get(seq)
            if not move_id:
                continue  # delivered before this instance existed, or unknown
            family = _move_family(move_id)
            if family is None:
                self._append_scored(seq, move_id, None, "unscored", logf)
                continue
            self.pending_scores[seq] = {
                "move_id": move_id,
                "family": family,
                "start_event_count": self.state.total_tool_event_count,
                "baseline_tool_class": self.recent_events[-1][1] if self.recent_events else None,
            }
        self.last_consumed_seq = consumed

    def _score_pending(self, logf):
        if not self.pending_scores:
            return
        for seq, info in list(self.pending_scores.items()):
            window = [e for e in self.recent_events if e[0] > info["start_event_count"]]
            success = self._family_succeeded(info["family"], info["baseline_tool_class"], window)
            if success:
                self._append_scored(seq, info["move_id"], info["family"], "success", logf)
                del self.pending_scores[seq]
            elif len(window) >= SCORE_WINDOW_EVENTS:
                self._append_scored(seq, info["move_id"], info["family"], "failure", logf)
                del self.pending_scores[seq]
            # else: not enough events yet — leave pending, check again next poll

    @staticmethod
    def _family_succeeded(family, baseline_tool_class, window):
        window = window[:SCORE_WINDOW_EVENTS]
        if family == "verify-claim":
            return any(
                any(sig in f"{tool_class} {target}".lower() for sig in VERIFY_CLAIM_SIGNALS)
                for _ec, tool_class, _status, target in window
            )
        if family == "thrash":
            return any(status == "ok" for _ec, _tool_class, status, _target in window)
        if family == "circling":
            return any(tool_class != baseline_tool_class for _ec, tool_class, _status, _target in window)
        return False

    def _append_scored(self, seq, move_id, family, outcome, logf):
        valve.append_telemetry(
            {
                "ts": time.time(),
                "session_id": self.session_id,
                "event": "scored",
                "seq": seq,
                "move_id": move_id,
                "family": family,
                "outcome": outcome,
            }
        )
        _log(logf, f"scored seq={seq} move={move_id} outcome={outcome}")
        if outcome in ("success", "failure"):
            self._maybe_auto_mute(move_id, logf)

    # ---- auto-mute ----

    @staticmethod
    def _mute_path(move_id):
        return os.path.join(MUTES_DIR, move_id.replace("/", "__") + ".json")

    def _is_muted(self, move_id):
        path = self._mute_path(move_id)
        try:
            with open(path, encoding="utf-8") as f:
                mute = json.load(f)
        except (OSError, json.JSONDecodeError):
            return False
        if mute.get("unmute_at", 0) <= time.time():
            try:
                os.remove(path)
            except OSError:
                pass
            return False
        return True

    @staticmethod
    def _scored_stats(move_id):
        """Scored fires + successes for `move_id`, read from the shared
        telemetry.jsonl (mute state must survive an observer restart —
        DESIGN.md §4b — so this reads durable state, not in-process counters,
        and reflects every session's fires, not just this one's)."""
        scored_fires = 0
        successes = 0
        try:
            with open(valve.TELEMETRY_PATH, encoding="utf-8") as f:
                for line in f:
                    try:
                        rec = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if rec.get("event") != "scored" or rec.get("move_id") != move_id:
                        continue
                    outcome = rec.get("outcome")
                    if outcome not in ("success", "failure"):
                        continue
                    scored_fires += 1
                    if outcome == "success":
                        successes += 1
        except OSError:
            pass
        return scored_fires, successes

    def _maybe_auto_mute(self, move_id, logf):
        if self._is_muted(move_id):
            return
        scored_fires, successes = self._scored_stats(move_id)
        if scored_fires < MUTE_FIRE_THRESHOLD or successes > 0:
            return
        os.makedirs(MUTES_DIR, exist_ok=True)
        now = time.time()
        mute = {
            "move_id": move_id,
            "muted_at": now,
            "unmute_at": now + MUTE_DURATION_S,
            "scored_fires": scored_fires,
            "successes": successes,
        }
        try:
            tmp = f"{self._mute_path(move_id)}.tmp.{os.getpid()}"
            with open(tmp, "w", encoding="utf-8") as f:
                json.dump(mute, f)
            os.replace(tmp, self._mute_path(move_id))
        except OSError:
            return
        valve.append_telemetry({"ts": now, "session_id": self.session_id, "event": "auto_mute", **mute})
        _log(logf, f"auto-muted {move_id}: {scored_fires} scored fires, 0 successes -> 7 days")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--session-id", required=True)
    ap.add_argument("--transcript", required=True)
    args = ap.parse_args()
    Daemon(args.session_id, args.transcript).run()


if __name__ == "__main__":
    main()
