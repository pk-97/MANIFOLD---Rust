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
import re
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

HABIT_MEMORY_WINDOW_S = 7 * 86400  # DESIGN.md §4c-2: trailing 7 days, all sessions

# DESIGN.md §2d: phase-transition shadow tier. TASK_DIAGNOSIS_RE is the
# "cheap regex" the spec calls for — deliberately crude, a known risk
# measured by shadow mode before delivery, not tuned here.
TASK_DIAGNOSIS_RE = re.compile(r"\b(?:fix|bug|broken|why|crash|wrong)\b", re.IGNORECASE)

# "a short window span" (DESIGN.md §2d rule 3) is not numerically specced —
# a placeholder judgment call, same status as the §2g card-limit bounds:
# pass 2 tunes these from shadow telemetry rather than a guess made now.
PHASE_OSCILLATION_SPAN_EVENTS = 40
PHASE_OSCILLATION_MIN_FLIPS = 3


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
        self.fire_ordinal = {}
        self.escalated = False
        self.next_seq = 1
        self.phase = "orienting"
        self.paths_seen = set()  # file paths Read/Written this worker's life (unread-edit)
        # DESIGN.md §2d: phase-transition shadow tier. In-memory only — workers
        # never get firestate persistence for anything (same scope decision
        # as §4b/§4c above); a worker exiting simply loses its phase history.
        self.phase_history = []  # [(end_event_count, phase)]
        self.phase_oscillation_active = False  # edge-trigger latch for rule 3


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
        self.fire_ordinal = {}  # effective move_id -> nth fire this session (§4c-3b)
        self.escalated = False  # escalate/checkpoint fires at most once per session
        self.next_seq = 1  # re-seeded in _run from the persistent consumed marker
        self.phase = "orienting"
        self.owns_pidfile = False
        # DESIGN.md §2d: phase-transition shadow tier. Unlike the §2f session
        # facts, this genuinely can't be rebuilt by catchup — catchup replays
        # with classify=False, so past windows' classifier verdicts (and
        # therefore their phases) are never re-issued. Firestate-persisted
        # below so an idle-exit revive doesn't amnesia the phase stream.
        self.phase_history = []  # [(end_event_count, phase)]
        self.phase_oscillation_active = False  # edge-trigger latch for rule 3
        # Sleep pass 1: cooldown/escalation state was daemon-memory only, so
        # an idle-exit + revive re-fired the same move on the same evidence
        # (two confirmed redelivery pairs in the graded week). Persisted per
        # session; event counts stay comparable across revives because
        # catchup rebuilds the same event_count from the transcript.
        self.fire_state_path = os.path.join(VERDICTS_DIR, f"{session_id}.firestate.json")
        self._load_fire_state()
        # Priming tier (sleep pass 1): file paths Read/Written this session.
        # Populated during catchup too, so a resumed session doesn't treat
        # every previously-read file as unread.
        self.paths_seen = set()

        # DESIGN.md §4b: outcome scoring + auto-mute.
        self.fire_records = {}  # seq -> move_id, for every flag *this instance* raised
        # DESIGN.md §4c-3b: fatigue ordinal — nth fire of `move_id` this session,
        # keyed by seq (parallel to fire_records) so a later scoring pass can
        # attach it without changing fire_records' shape.
        self.fire_ordinals = {}  # seq -> ordinal
        self.pending_scores = {}  # seq -> {move_id, start_event_count, baseline_tool_class}
        self.recent_events = collections.deque(maxlen=64)  # (event_count, tool_class, status)
        self.last_consumed_seq = 0  # re-seeded in _run; tracks delivery via the consumed marker

        # DESIGN.md §4c-2: habit memory. Seeded from telemetry.jsonl once, in
        # _run ("at observer start"); _resolve_fire increments this in-memory
        # copy per fire so a live session's own fires count without re-
        # scanning the file each time. Session-only, same scope decision as
        # §4b (see the fire_records comment above).
        self.weekly_fire_counts = {}  # move_id -> count, trailing HABIT_MEMORY_WINDOW_S

        # DESIGN.md §2b: worker nudges, shipped OFF (see _worker_nudges_enabled).
        # Subagent transcripts live under <project_dir>/<session_id>/subagents/,
        # NOT next to the session transcript — dirname(transcript) alone scanned
        # a nonexistent dir and discovery never found a single worker (the
        # 2026-07-04 Opus orchestration ran fully unobserved with the flag ON).
        self.subagents_dir = os.path.join(os.path.dirname(transcript_path), session_id, "subagents")
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
        self.weekly_fire_counts = self._rollup_weekly_fires()
        _log(
            logf,
            f"observer started, pid={os.getpid()}, next_seq={self.next_seq}, "
            f"weekly_baseline={self.weekly_fire_counts}, transcript={self.transcript_path}",
        )

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
        subdir = self.subagents_dir
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
                # Mirrors _feed_line's peek: no recent_events/outcome-scoring
                # ledger for workers (§4b stays session-only), but stopgap
                # detection (§2c) is deterministic per-event and applies here
                # too — bandaids concentrate in worker edits, per the
                # migration-shortcut precedent this feature exists to catch.
                count_before = worker.state.total_tool_event_count
                peeked = self._peek_tool_events(worker.state, content)
                closed, _human = worker.state.feed_user_content(content, ts)
                for idx, (name, input_, _status) in enumerate(peeked):
                    event_count = count_before + idx + 1
                    # Same per-event priority as _feed_line: specific beats
                    # generic, primer last (it retries until delivered).
                    if classify:
                        self._check_stopgap(name, input_, event_count, logf, mailbox=worker)
                        self._check_design_primer(name, input_, event_count, logf, mailbox=worker)
                    self._check_unread_edit(name, input_, event_count, logf, mailbox=worker, live=classify)
                    if classify:
                        self._check_primer(event_count, logf, mailbox=worker)
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
                peeked = self._peek_tool_events(self.state, content)
                closed, _human = self.state.feed_user_content(content, ts)
                for idx, (name, input_, status) in enumerate(peeked):
                    label = common.tool_label(name, input_, self.state.session_model)
                    target = common.tool_target(input_)
                    event_count = count_before + idx + 1
                    self.recent_events.append((event_count, _tool_class(label), status, target))
                    # Per-event priority: specific detections (stopgap,
                    # git-landing) beat the generic unread-edit, and the
                    # primer goes last — it retries until delivered, the
                    # others are one-shot signals for THIS event.
                    if classify:
                        self._check_stopgap(name, input_, event_count, logf)
                        self._check_git_landing(name, input_, event_count, logf)
                        self._check_design_primer(name, input_, event_count, logf)
                    # live=classify: catchup populates paths_seen, never fires
                    self._check_unread_edit(name, input_, event_count, logf, live=classify)
                    if classify:
                        self._check_primer(event_count, logf)
        if closed and classify:
            self._handle_window(closed, logf)

    def _peek_tool_events(self, state, content):
        """Read-only pass over tool_result blocks -> [(name, input, status)],
        in the same order common.WindowState.feed_user_content will consume
        them. Must run BEFORE that call, which pops state.pending. Takes an
        explicit `state` so the same helper serves both the session's own
        WindowState and a worker's (§2b)."""
        events = []
        if not isinstance(content, list):
            return events
        for c in content:
            if isinstance(c, dict) and c.get("type") == "tool_result":
                name, input_ = state.pending.get(c.get("tool_use_id"), ("?", {}))
                events.append((name, input_, common.tool_result_status(c)))
        return events

    # ---- stopgap detection (DESIGN.md §2c tier 1) ----

    def _check_stopgap(self, name, input_, event_count, logf, mailbox=None):
        """Deterministic, never the classifier: a live-tailed Edit/Write/
        MultiEdit that adds a confession marker fires mechanical/confessed-
        stopgap through the normal mailbox/cooldown path (`_resolve_fire`),
        exactly like any other flag — just selected by regex instead of
        Haiku. Never runs during catchup (`classify=False` callers never
        reach this)."""
        hits = common.detect_stopgap_markers(name, input_)
        if not hits:
            return
        mb = mailbox if mailbox is not None else self
        verdict = {"evidence": f"{name} adds: {', '.join(hits)}", "confidence": 1.0}
        flag_out = self._resolve_fire(event_count, "mechanical/confessed-stopgap", verdict, logf, mailbox=mailbox)
        if not flag_out:
            return
        record = {
            "ts": time.time(),
            "phase": mb.phase,
            "window_version": common.WINDOW_VERSION,
            "flag": flag_out,
        }
        _atomic_write_json(mb.verdict_path, record)
        _log(logf, f"mechanical/confessed-stopgap fired: {name} {common.tool_target(input_)} hits={hits}")

    # ---- priming tier (sleep pass 1: pre-authored advice at predictable
    # moments — no detection, no classifier, no false-positive budget) ----

    def _check_primer(self, event_count, logf, mailbox=None):
        """mechanical/reasoning-primer: fire on the first live tool event of a
        target (main session or worker), then re-arm every advice-recur events
        (§2e, Peter 2026-07-05) so long orchestration/worker runs get the
        advice back into context after it scrolls out. Firestate persistence
        (main) keeps the gate across revives; if another whisper is pending,
        _resolve_fire declines and this retries on the next event."""
        mb = mailbox if mailbox is not None else self
        # Fast path only — _resolve_fire re-checks the same advice-recur gate.
        prev = mb.last_fire_event.get("mechanical/reasoning-primer")
        if prev is not None and (event_count - prev) < common.COOLDOWN_EVENTS["advice-recur"]:
            return
        verdict = {"evidence": "first live tool event (priming tier)", "confidence": 1.0}
        flag_out = self._resolve_fire(event_count, "mechanical/reasoning-primer", verdict, logf, mailbox=mailbox)
        if not flag_out:
            return
        record = {
            "ts": time.time(),
            "phase": mb.phase,
            "window_version": common.WINDOW_VERSION,
            "flag": flag_out,
        }
        _atomic_write_json(mb.verdict_path, record)
        _log(logf, "mechanical/reasoning-primer fired (priming tier)")

    DESIGN_DOC_RE = re.compile(r"_(?:DESIGN|PLAN)\.md$")

    def _check_design_primer(self, name, input_, event_count, logf, mailbox=None):
        """mechanical/design-primer: a live Write/Edit of a *_DESIGN.md or
        *_PLAN.md — design-taste advice at the moment a design is being
        authored. Re-arms every advice-recur events per target (§2e); retry
        semantics identical to reasoning-primer."""
        mb = mailbox if mailbox is not None else self
        # Fast path only — _resolve_fire re-checks the same advice-recur gate.
        prev = mb.last_fire_event.get("mechanical/design-primer")
        if prev is not None and (event_count - prev) < common.COOLDOWN_EVENTS["advice-recur"]:
            return
        if name not in ("Write", "Edit", "MultiEdit") or not isinstance(input_, dict):
            return
        path = input_.get("file_path") or ""
        if not self.DESIGN_DOC_RE.search(path):
            return
        verdict = {"evidence": f"{name} {path} (design doc, priming tier)", "confidence": 1.0}
        flag_out = self._resolve_fire(event_count, "mechanical/design-primer", verdict, logf, mailbox=mailbox)
        if not flag_out:
            return
        record = {
            "ts": time.time(),
            "phase": mb.phase,
            "window_version": common.WINDOW_VERSION,
            "flag": flag_out,
        }
        _atomic_write_json(mb.verdict_path, record)
        _log(logf, f"mechanical/design-primer fired: {name} {path}")

    def _check_unread_edit(self, name, input_, event_count, logf, mailbox=None, live=True):
        """mechanical/unread-edit: an Edit/MultiEdit to a path this target has
        never Read or Written. Called for EVERY tool event including catchup
        (live=False) so the seen-path set stays complete; only live edits can
        fire. Check-then-add: the current edit must not vouch for itself."""
        if not isinstance(input_, dict):
            return
        path = input_.get("file_path") or input_.get("notebook_path")
        if not path or name not in ("Read", "Edit", "Write", "MultiEdit", "NotebookEdit"):
            return
        mb = mailbox if mailbox is not None else self
        unseen = path not in mb.paths_seen
        mb.paths_seen.add(path)
        if not (live and unseen and name in ("Edit", "MultiEdit")):
            return
        if common.STOPGAP_EXCLUDED_PATH_RE.search(path):
            return  # .md and .claude/ internals, same exclusions as stopgap
        verdict = {"evidence": f"{name} {path} — never read this session", "confidence": 1.0}
        flag_out = self._resolve_fire(event_count, "mechanical/unread-edit", verdict, logf, mailbox=mailbox)
        if not flag_out:
            return
        record = {
            "ts": time.time(),
            "phase": mb.phase,
            "window_version": common.WINDOW_VERSION,
            "flag": flag_out,
        }
        _atomic_write_json(mb.verdict_path, record)
        _log(logf, f"mechanical/unread-edit fired: {name} {path}")

    def _check_git_landing(self, name, input_, event_count, logf, mailbox=None):
        """Deterministic, never the classifier: a Bash cherry-pick or
        branch-delete fires mechanical/git-landing through the normal
        mailbox/cooldown path (`_resolve_fire`) — the two twin-commit-prone
        operations .claude/GIT_TREE_DISCIPLINE.md §2's push/merge hook guard
        doesn't cover. Peter's own suggestion, 2026-07-04."""
        hits = common.detect_git_landing_signal(name, input_)
        if not hits:
            return
        mb = mailbox if mailbox is not None else self
        verdict = {"evidence": f"{name}: {', '.join(hits)}", "confidence": 1.0}
        flag_out = self._resolve_fire(event_count, "mechanical/git-landing", verdict, logf, mailbox=mailbox)
        if not flag_out:
            return
        record = {
            "ts": time.time(),
            "phase": mb.phase,
            "window_version": common.WINDOW_VERSION,
            "flag": flag_out,
        }
        _atomic_write_json(mb.verdict_path, record)
        _log(logf, f"mechanical/git-landing fired: {name} {common.tool_target(input_)} hits={hits}")

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
        # Conditional Stop-wait (sleep pass 1): mark "classification in
        # flight" so the Stop hook can wait a couple of seconds ONLY when a
        # verdict is genuinely about to land — the chat-lag fix Peter asked
        # for, without the flat per-turn wait he rejected. Main session only
        # (chat lag is a main-turn phenomenon; worker Stops shouldn't pay).
        marker = None
        if mailbox is None:
            marker = os.path.join(VERDICTS_DIR, f"{self.session_id}.classifying")
            try:
                with open(marker, "w", encoding="utf-8") as f:
                    f.write(str(time.time()))
            except OSError:
                marker = None
        try:
            verdict = common.call_classifier(self.system_prompt, window["text"])
        finally:
            if marker:
                try:
                    os.remove(marker)
                except OSError:
                    pass
        if "error" in verdict:
            _log(logf, f"classifier error: {verdict['error']}")
            return  # fail open — leave the verdict file as it was

        mb.phase = verdict.get("phase") or mb.phase
        raw_flag = verdict.get("flag")
        move_id = common.validate_move_id(raw_flag, self.moves)
        if raw_flag and not move_id:
            _log(logf, f"rejected unknown/invalid move id from classifier: {raw_flag!r}")

        flag_out = self._resolve_fire(window["end_event_count"], move_id, verdict, logf, mailbox=mailbox) if move_id else None

        # DESIGN.md §2d: phase-transition shadow tier. Independent of whatever
        # the classifier flagged this window — zero extra classifier cost,
        # runs off the phase it already emitted.
        self._check_phase_transitions(mailbox, window["end_event_count"], logf)

        record = {
            "ts": time.time(),
            "window_range": {"end_event_count": window["end_event_count"], "end_ts": window["end_ts"]},
            "phase": mb.phase,
            "window_version": common.WINDOW_VERSION,
        }
        if flag_out:
            record["flag"] = flag_out
        else:
            # Never clobber an undelivered whisper with null — DESIGN.md
            # invariant 3 is "one whisper at a time", not "zero eventually".
            prior = self._read_verdict_file(mailbox)
            record["flag"] = prior["flag"] if prior and prior.get("flag") else None
        _atomic_write_json(mb.verdict_path, record)

    # ---- phase-transition tier (DESIGN.md §2d, shadow mode) ----
    #
    # SHADOW MODE ONLY: these rules log `phase_fire` telemetry and deliver
    # NOTHING — no mailbox/verdict write, no _resolve_fire, no cooldown or
    # escalation bookkeeping. Pass 2 hand-grades the shadow fires and flips
    # delivery on per-rule once shadow precision clears 60%; until then this
    # tier cannot reach the model by construction (no code path writes a
    # verdict file from here).

    def _check_phase_transitions(self, mailbox, event_count, logf):
        mb = mailbox if mailbox is not None else self
        history_before = list(mb.phase_history)
        prev_phase = history_before[-1][1] if history_before else None
        current_phase = mb.phase

        if current_phase == "implementing" and prev_phase != "implementing":
            self._check_phase_rule_investigate(mb, history_before, event_count, logf)
        if current_phase == "reporting" and prev_phase != "reporting":
            self._check_phase_rule_verify(mb, history_before, event_count, logf)

        mb.phase_history.append((event_count, current_phase))
        self._check_phase_rule_oscillation(mb, event_count, logf)

        if mailbox is None:
            self._save_fire_state()

    def _check_phase_rule_investigate(self, mb, history_before, event_count, logf):
        """phase/implementing-without-investigating: TASK reads as a
        diagnosis (cheap regex — the TASK-shape heuristic is deliberately
        crude, per DESIGN.md §2d) and phase enters `implementing` with zero
        `investigating` windows since TASK was last set — building before
        looking, caught at the transition instead of after the fact."""
        task_text = mb.state.current_task or ""
        if not TASK_DIAGNOSIS_RE.search(task_text):
            return
        task_set_event = mb.state.total_tool_event_count - mb.state.events_since_task
        since_task_phases = [p for ec, p in history_before if ec > task_set_event]
        if "investigating" in since_task_phases:
            return
        self._log_phase_fire(
            mb,
            "phase/implementing-without-investigating",
            event_count,
            logf,
            f"diagnosis-shaped TASK {task_text[:80]!r}; entered implementing with no "
            f"investigating window since task was set (event {task_set_event})",
        )

    def _check_phase_rule_verify(self, mb, history_before, event_count, logf):
        """phase/no-verify-before-reporting: phase enters `reporting` with
        zero `verifying` windows since the last `implementing` window —
        fires on the transition, before any done-claim text exists (verify-
        claim's blind spot). No prior `implementing` window this session
        means nothing to gate against, so the rule is silent."""
        last_impl_idx = None
        for i in range(len(history_before) - 1, -1, -1):
            if history_before[i][1] == "implementing":
                last_impl_idx = i
                break
        if last_impl_idx is None:
            return
        since_impl_phases = [p for _ec, p in history_before[last_impl_idx + 1 :]]
        if "verifying" in since_impl_phases:
            return
        self._log_phase_fire(
            mb,
            "phase/no-verify-before-reporting",
            event_count,
            logf,
            "entered reporting with no verifying window since the last implementing window",
        )

    def _check_phase_rule_oscillation(self, mb, event_count, logf):
        """phase/stuck-oscillation: implementing<->stuck flips at least
        PHASE_OSCILLATION_MIN_FLIPS times inside the trailing
        PHASE_OSCILLATION_SPAN_EVENTS window — coaching/differential's
        moment, detected structurally instead of from wording. Edge-
        triggered (fires once per rising edge, not once per window the
        condition continues to hold) so a long oscillating stretch doesn't
        flood shadow telemetry with the same specimen."""
        span_start = event_count - PHASE_OSCILLATION_SPAN_EVENTS
        filtered = [p for ec, p in mb.phase_history if ec >= span_start and p in ("implementing", "stuck")]
        flips = sum(1 for i in range(1, len(filtered)) if filtered[i] != filtered[i - 1])
        firing_now = flips >= PHASE_OSCILLATION_MIN_FLIPS
        if firing_now and not mb.phase_oscillation_active:
            self._log_phase_fire(
                mb,
                "phase/stuck-oscillation",
                event_count,
                logf,
                f"{flips} implementing<->stuck flips in the last {PHASE_OSCILLATION_SPAN_EVENTS} events",
            )
        mb.phase_oscillation_active = firing_now

    def _log_phase_fire(self, mailbox, rule_id, event_count, logf, evidence):
        """Shadow-mode delivery: telemetry only, per DESIGN.md §2d — never a
        verdict-file write, never `_resolve_fire`."""
        valve.append_telemetry(
            {
                "ts": time.time(),
                "session_id": self.session_id,
                "agent_id": getattr(mailbox, "agent_id", None),
                "event": "phase_fire",
                "move_id": rule_id,
                "event_count": event_count,
                "evidence": evidence,
            }
        )
        _log(logf, f"phase_fire (shadow, not delivered): {rule_id} @ {event_count}: {evidence}")

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

    def _session_fp_grades(self):
        """move_id -> count of this session's own self-grades marked FP, read
        from the gitignored live_grades.session*.jsonl files. Used only to
        discount the escalation counter — a whisper the session already judged
        wrong shouldn't march it toward escalate/checkpoint. Fails open to {}."""
        counts = {}
        try:
            import glob

            for path in glob.glob(os.path.join(DAEMON_DIR, "eval", "live_grades.session*.jsonl")):
                with open(path, encoding="utf-8") as f:
                    for line in f:
                        line = line.strip()
                        if not line:
                            continue
                        try:
                            rec = json.loads(line)
                        except json.JSONDecodeError:
                            continue
                        if rec.get("session_id") != self.session_id:
                            continue
                        if str(rec.get("correct", "")).upper() == "FP":
                            mid = rec.get("move_id")
                            if mid:
                                counts[mid] = counts.get(mid, 0) + 1
        except OSError:
            pass
        return counts

    def _load_fire_state(self):
        try:
            with open(self.fire_state_path, encoding="utf-8") as f:
                st = json.load(f)
        except (OSError, json.JSONDecodeError):
            return
        self.last_fire_event = {k: int(v) for k, v in (st.get("last_fire_event") or {}).items()}
        self.fire_count = {k: int(v) for k, v in (st.get("fire_count") or {}).items()}
        self.fire_ordinal = {k: int(v) for k, v in (st.get("fire_ordinal") or {}).items()}
        self.escalated = bool(st.get("escalated", False))
        # DESIGN.md §2d: phase_history can't be rebuilt by catchup (past
        # windows' classifier verdicts are never re-issued) — unlike the §2f
        # session facts, this is genuinely daemon-only memory.
        self.phase_history = [(int(ec), p) for ec, p in (st.get("phase_history") or [])]
        self.phase_oscillation_active = bool(st.get("phase_oscillation_active", False))

    def _save_fire_state(self):
        try:
            tmp = f"{self.fire_state_path}.tmp.{os.getpid()}"
            with open(tmp, "w", encoding="utf-8") as f:
                json.dump(
                    {
                        "last_fire_event": self.last_fire_event,
                        "fire_count": self.fire_count,
                        "fire_ordinal": self.fire_ordinal,
                        "escalated": self.escalated,
                        "phase_history": self.phase_history,
                        "phase_oscillation_active": self.phase_oscillation_active,
                    },
                    f,
                )
            os.replace(tmp, self.fire_state_path)
        except OSError:
            pass

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

        # Sleep pass 1: escalation counted raw fires, so three FPs escalated
        # into a fourth FP (checkpoint inherits the precision of the fires
        # beneath it). Discount fires this session already self-graded FP.
        fp_graded = self._session_fp_grades()
        effective_fires = mb.fire_count[move_id] - fp_graded.get(move_id, 0)
        # Advice-kind moves (§2e priming tier) recur by design — repeat fires
        # are the schedule working, not habituation, so they never escalate.
        is_advice = self.moves.get(move_id, {}).get("kind") == "advice"
        if not is_advice and effective_fires > ESCALATE_AFTER and not mb.escalated and "escalate/checkpoint" in self.moves:
            effective_id = "escalate/checkpoint"
            mb.escalated = True
            _log(logf, f"escalating {move_id} -> escalate/checkpoint after {mb.fire_count[move_id]} fires ({fp_graded.get(move_id, 0)} self-graded FP, discounted)")

        # DESIGN.md §4c-3b: fatigue ordinal — nth fire of the move that's
        # actually recorded downstream (post-escalation), this session.
        mb.fire_ordinal[effective_id] = mb.fire_ordinal.get(effective_id, 0) + 1
        ordinal = mb.fire_ordinal[effective_id]

        seq = mb.next_seq
        mb.next_seq += 1
        # DESIGN.md §4c-2: habit memory. Session-only, same reasoning as the
        # §4b comment above (agent mailboxes don't get scored yet either).
        weekly_count = None
        if mailbox is None:
            self.weekly_fire_counts[effective_id] = self.weekly_fire_counts.get(effective_id, 0) + 1
            weekly_count = self.weekly_fire_counts[effective_id]

        if mailbox is None:
            # §4b outcome scoring is session-only for now (item 3's scope is
            # delivery infra, not scoring) — and mailbox.next_seq is a
            # separate counter per agent, so recording it here under the
            # session's shared self.fire_records would collide seq numbers
            # across mailboxes.
            self.fire_records[seq] = effective_id
            self.fire_ordinals[seq] = ordinal
            self._save_fire_state()  # survive idle-exit revives (redelivery fix)
        return {
            "move_id": effective_id,
            "evidence": verdict.get("evidence"),
            "confidence": verdict.get("confidence"),
            "seq": seq,
            "weekly_count": weekly_count,
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
            ordinal = self.fire_ordinals.get(seq)  # §4c-3b: None for pre-this-instance fires
            family = _move_family(move_id)
            if family is None:
                self._append_scored(seq, move_id, None, "unscored", logf, ordinal=ordinal)
                continue
            self.pending_scores[seq] = {
                "move_id": move_id,
                "family": family,
                "ordinal": ordinal,
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
                self._append_scored(seq, info["move_id"], info["family"], "success", logf, ordinal=info["ordinal"])
                del self.pending_scores[seq]
            elif len(window) >= SCORE_WINDOW_EVENTS:
                self._append_scored(seq, info["move_id"], info["family"], "failure", logf, ordinal=info["ordinal"])
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

    def _append_scored(self, seq, move_id, family, outcome, logf, ordinal=None):
        valve.append_telemetry(
            {
                "ts": time.time(),
                "session_id": self.session_id,
                "event": "scored",
                "seq": seq,
                "move_id": move_id,
                "family": family,
                "outcome": outcome,
                "ordinal": ordinal,  # DESIGN.md §4c-3b: nth fire of move_id this session
            }
        )
        _log(logf, f"scored seq={seq} move={move_id} outcome={outcome} ordinal={ordinal}")
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

    # ---- habit memory (DESIGN.md §4c-2) ----

    @staticmethod
    def _rollup_weekly_fires():
        """Per-move delivery counts across ALL sessions, trailing
        HABIT_MEMORY_WINDOW_S, read once at observer start from the shared
        telemetry.jsonl. Counts "scored" records (success/failure/unscored)
        as the fire proxy — every delivered flag produces exactly one,
        whether or not its family has a mechanical outcome (§4b). Like
        fire_count, this is a habituation signal, not a safety-critical
        count: a baseline that's up to one observer-lifetime stale is an
        acceptable trade for never re-scanning the whole file mid-session."""
        counts = {}
        cutoff = time.time() - HABIT_MEMORY_WINDOW_S
        try:
            with open(valve.TELEMETRY_PATH, encoding="utf-8") as f:
                for line in f:
                    try:
                        rec = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if rec.get("event") != "scored" or rec.get("ts", 0) < cutoff:
                        continue
                    move_id = rec.get("move_id")
                    if not move_id:
                        continue
                    counts[move_id] = counts.get(move_id, 0) + 1
        except OSError:
            pass
        return counts


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--session-id", required=True)
    ap.add_argument("--transcript", required=True)
    args = ap.parse_args()
    Daemon(args.session_id, args.transcript).run()


if __name__ == "__main__":
    main()
