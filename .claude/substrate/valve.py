"""Shared mailbox logic for the substrate valve hooks (PostToolUse,
UserPromptSubmit). Both hooks check the same verdict file and the same
consumed-seq marker per session — whichever fires first delivers a pending
flag and bumps the marker; the other sees it already consumed and is a
no-op. That shared marker is what gives "one whisper at a time" (DESIGN.md
invariant 3) without any file locking.

Every function here fails open (returns None/False, never raises) — a
substrate bug must never surface as a blocked or slowed session.
"""

import json
import os
import time

SUBSTRATE_DIR = os.path.dirname(os.path.abspath(__file__))
VERDICTS_DIR = os.path.join(SUBSTRATE_DIR, "verdicts")
MOVES_PATH = os.path.join(SUBSTRATE_DIR, "moves.md")
TELEMETRY_PATH = os.path.join(SUBSTRATE_DIR, "telemetry.jsonl")

VERDICT_MAX_AGE = 300  # 5 min — DESIGN.md invariant 1: a stale verdict is treated as absent

_PAYLOAD_CACHE = None


def _payloads():
    global _PAYLOAD_CACHE
    if _PAYLOAD_CACHE is None:
        import sys

        sys.path.insert(0, SUBSTRATE_DIR)
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


def build_block(flag):
    move_id = flag.get("move_id")
    payload = (_payloads().get(move_id) or {}).get("payload")
    if not payload:
        return None
    return (
        f'<substrate move="{move_id}" unvalidated="true" confidence="{flag.get("confidence")}">\n'
        f"{payload}\n"
        f"</substrate>"
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
        observer = os.path.join(SUBSTRATE_DIR, "observer.py")
        if not os.path.exists(observer):
            return
        import subprocess
        import sys

        os.makedirs(VERDICTS_DIR, exist_ok=True)
        log_path = os.path.join(VERDICTS_DIR, f"{session_id}.log")
        with open(log_path, "a", encoding="utf-8") as log:
            subprocess.Popen(
                [sys.executable, os.path.join(SUBSTRATE_DIR, "observer.py"), "--session-id", session_id, "--transcript", transcript_path],
                stdout=log,
                stderr=log,
                stdin=subprocess.DEVNULL,
                start_new_session=True,
                cwd=SUBSTRATE_DIR,
            )
        append_telemetry({"ts": time.time(), "session_id": session_id, "event": "observer_spawn"})
    except Exception:
        pass


def pending_injection(session_id):
    """Returns (block_text, seq) for an unconsumed, valid, fresh flag, or
    (None, None) if there's nothing to deliver. Never raises."""
    try:
        verdict = read_verdict(session_id)
        if not verdict:
            return None, None
        flag = verdict.get("flag")
        if not flag:
            return None, None
        seq = flag.get("seq")
        if seq is None or read_consumed(session_id) >= seq:
            return None, None
        block = build_block(flag)
        if not block:
            return None, None
        return block, seq
    except Exception:
        return None, None
