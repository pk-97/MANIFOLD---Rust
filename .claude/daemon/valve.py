"""Shared mailbox logic for the daemon valve hooks (PostToolUse,
UserPromptSubmit). Both hooks check the same verdict file and the same
consumed-seq marker per session — whichever fires first delivers a pending
flag and bumps the marker; the other sees it already consumed and is a
no-op. That shared marker is what gives "one whisper at a time" (DESIGN.md
invariant 3) without any file locking.

Every function here fails open (returns None/False, never raises) — a
daemon bug must never surface as a blocked or slowed session.
"""

import json
import os
import time

DAEMON_DIR = os.path.dirname(os.path.abspath(__file__))
# Env-var overrides (2026-07-07, T11): a subprocess-spawned hook does a fresh
# `import valve` in a fresh interpreter, so a parent test process's module-
# attribute monkeypatching can never reach it — three tests that spawn the
# real hooks as subprocesses were writing straight into the REAL verdicts dir
# and telemetry.jsonl (3 manual purges, 30+10+9 residue records). Set before
# the subprocess starts (via `env=`) and picked up here at import time.
VERDICTS_DIR = os.environ.get("DAEMON_VERDICTS_DIR") or os.path.join(DAEMON_DIR, "verdicts")
MOVES_PATH = os.path.join(DAEMON_DIR, "moves.md")  # unchanged — read-only, no leakage risk
TELEMETRY_PATH = os.environ.get("DAEMON_TELEMETRY_PATH") or os.path.join(DAEMON_DIR, "telemetry.jsonl")
EVAL_DIR = os.path.join(DAEMON_DIR, "eval")
WORKER_NUDGES_FLAG = os.path.join(VERDICTS_DIR, "worker-nudges.enabled")

VERDICT_MAX_AGE = 300  # 5 min — DESIGN.md invariant 1: a stale verdict is treated as absent


def worker_nudges_enabled():
    """DESIGN.md §2b, shipped OFF: absent flag file = fully dark. Nobody
    creates this file — the caller (PostToolUse hook) must keep returning
    immediately for agent-tagged events while this is False, exactly like
    before the feature existed."""
    return os.path.exists(WORKER_NUDGES_FLAG)

_PAYLOAD_CACHE = None


def _payloads():
    global _PAYLOAD_CACHE
    if _PAYLOAD_CACHE is None:
        import sys

        sys.path.insert(0, DAEMON_DIR)
        import common

        _PAYLOAD_CACHE = common.parse_moves(common.read(MOVES_PATH))
    return _PAYLOAD_CACHE


def _verdict_path(session_id):
    return os.path.join(VERDICTS_DIR, f"{session_id}.json")


def _consumed_path(session_id):
    return os.path.join(VERDICTS_DIR, f"{session_id}.consumed")


def read_verdict(session_id):
    try:
        with open(_verdict_path(session_id), encoding="utf-8") as f:
            v = json.load(f)
    except (OSError, json.JSONDecodeError):
        return None
    if time.time() - v.get("ts", 0) > VERDICT_MAX_AGE:
        return None
    return v


def read_consumed(session_id):
    try:
        with open(_consumed_path(session_id), encoding="utf-8") as f:
            return int(f.read().strip() or "0")
    except (OSError, ValueError):
        return 0


def write_consumed(session_id, seq):
    try:
        os.makedirs(VERDICTS_DIR, exist_ok=True)
        tmp = f"{_consumed_path(session_id)}.tmp.{os.getpid()}"
        with open(tmp, "w", encoding="utf-8") as f:
            f.write(str(seq))
        os.replace(tmp, _consumed_path(session_id))
    except OSError:
        pass


def build_block(flag, agent_id=None):
    move_id = flag.get("move_id")
    entry = _payloads().get(move_id) or {}
    payload = entry.get("payload")
    if not payload:
        return None
    # §2e advice tier (Peter, 2026-07-05): the priming moves are scheduled
    # general advice, not detections. Framed so they never read as "you did
    # something wrong" — a distinct tag, an explicit nothing-is-wrong
    # preamble, no supervised-mode ack, no habit ordinal (both read as
    # accusation under an advice frame; advice fires are pass-graded from
    # downstream behavior, never self-graded). Wording frozen per
    # invariant 5 — sleep-pass edits only, same as the payloads.
    if entry.get("kind") == "advice":
        return (
            f'<daemon-advice move="{move_id}">\n'
            f"(Scheduled advice, not a detection — nothing you did triggered "
            f"this and nothing is wrong. It recurs in long sessions so these "
            f"patterns stay in context. Nothing to acknowledge or grade — "
            f"fold it in and carry on.)\n"
            f"\n"
            f"{payload}\n"
            f"</daemon-advice>"
        )
    # No unvalidated/confidence attributes in the model-facing tag: both are
    # licenses to discount the anchor (Peter's call, 2026-07-04). Confidence
    # stays in the verdict file for grading.
    #
    # DESIGN.md §4c-2: habit memory. Template wording is frozen (invariant-5
    # amendment) — only the ordinal varies, and it's mechanically computed by
    # the observer's weekly rollup (see observer.py _rollup_weekly_fires),
    # never composed here. weekly_count is None for fires this feature
    # doesn't cover yet (worker-nudges mailboxes) — no line in that case.
    weekly_count = flag.get("weekly_count")
    habit_note = ""
    if isinstance(weekly_count, int) and weekly_count > 0:
        import common

        habit_note = f"\n\n({common.ordinal(weekly_count)} fire of this move across sessions this week.)"
    # 2026-07-05 addressability fix: 55/74 self-graded records carried
    # seq:null because this sentence never told the session its own fire's
    # seq — the sleep pass then can't join the grade back to the exact fire
    # it was for. Only the numeral varies (habit-memory ordinal precedent);
    # everything else in this sentence is frozen, same invariant.
    seq = flag.get("seq")
    # DESIGN.md §2h.4: this same ack reaches worker deliveries too (both
    # PostToolUse and Stop route through here regardless of agent_id) — a
    # worker's grade line needs "agent_id" alongside "seq", or (session_id,
    # seq) alone collides across workers (RUNBOOK.md step 2). Only this
    # flag varies with the mailbox; the rest of the sentence is frozen.
    # 2026-07-13 (Peter): grading is one shot via log_grade.py — sessions no
    # longer read RUNBOOK step 2 for the record format; the script owns it.
    agent_flag = f" --agent-id {agent_id}" if agent_id else ""
    return (
        f'<daemon move="{move_id}">\n'
        f"{payload}"
        f"{habit_note}\n"
        f"\n"
        f"(Supervised mode: briefly acknowledge this note out loud in your next "
        f'message — one sentence, e.g. "daemon nudged me about {move_id} — '
        f'checking" — so Peter can judge whether the nudge was right. Before the '
        f"session ends, also log one self-grade per fire, one shot: "
        f"python3 .claude/daemon/log_grade.py {seq} {move_id} "
        f'<correct y/n> <effective y/n> "<one-sentence evidence>"{agent_flag} '
        f"— it owns the format and lands in the gitignored session grades file; "
        f"the sleep pass reads these as provisional and may override.)\n"
        f"</daemon>"
    )


def append_telemetry(record):
    try:
        os.makedirs(VERDICTS_DIR, exist_ok=True)
        with open(TELEMETRY_PATH, "a", encoding="utf-8") as f:
            f.write(json.dumps(record) + "\n")
    except OSError:
        pass


def observer_alive(session_id):
    try:
        with open(os.path.join(VERDICTS_DIR, f"{session_id}.pid"), encoding="utf-8") as f:
            pid = int(f.read().strip())
        os.kill(pid, 0)  # signal 0: existence check only
        return True
    except (OSError, ValueError):
        return False


def ensure_observer(session_id, transcript_path):
    """Spawn the observer daemon if one isn't already running for this
    session. Called by SessionStart (initial spawn) and by both valve hooks
    (revive): the daemon exits after 10 idle minutes, and session activity
    itself is what brings it back — catchup rebuilds its window state from
    the transcript, so nothing is lost but the idle gap. Cost when the
    daemon is alive: one pidfile read + one signal-0 check. Fails open."""
    try:
        if not session_id or not transcript_path:
            return
        if observer_alive(session_id):
            return
        observer = os.path.join(DAEMON_DIR, "observer.py")
        if not os.path.exists(observer):
            return
        import subprocess
        import sys

        os.makedirs(VERDICTS_DIR, exist_ok=True)
        log_path = os.path.join(VERDICTS_DIR, f"{session_id}.log")
        with open(log_path, "a", encoding="utf-8") as log:
            subprocess.Popen(
                [sys.executable, os.path.join(DAEMON_DIR, "observer.py"), "--session-id", session_id, "--transcript", transcript_path],
                stdout=log,
                stderr=log,
                stdin=subprocess.DEVNULL,
                start_new_session=True,
                cwd=DAEMON_DIR,
            )
        append_telemetry({"ts": time.time(), "session_id": session_id, "event": "observer_spawn"})
    except Exception:
        pass


def pending_injection(session_id, agent_id=None):
    """Returns (block_text, seq, move_id) for an unconsumed, valid, fresh
    flag, or (None, None, None) if there's nothing to deliver. move_id rides
    along so the delivering hook can stamp it into the `injected` telemetry
    record — sleep pass 1 had to recover move ids from surviving mailbox
    files because delivery records lacked them. `agent_id` (DESIGN.md §2h.4)
    only affects the ack sentence build_block appends — it plays no part in
    which mailbox is read; the caller already resolved that into `session_id`
    (which is actually the mailbox key: bare session id, or `<session>.
    <agent_id>` for a worker). Never raises."""
    try:
        verdict = read_verdict(session_id)
        if not verdict:
            return None, None, None
        flag = verdict.get("flag")
        if not flag:
            return None, None, None
        seq = flag.get("seq")
        if seq is None or read_consumed(session_id) >= seq:
            return None, None, None
        block = build_block(flag, agent_id=agent_id)
        if not block:
            return None, None, None
        return block, seq, flag.get("move_id")
    except Exception:
        return None, None, None
