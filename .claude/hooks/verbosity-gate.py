#!/usr/bin/env python3
"""Stop hook: block the turn when the final message is over budget.

Why this and not the response-style reminder: injected prose is advice, and
advice loses to everything else in context (Peter, 2026-07-30: "the hook does
not work. Ever."). This one is not advice — it reads the message that was just
written and sends it back to be rewritten.

Length is the proxy for padding. A long answer is long because it restates,
hedges, lists options nobody asked for, or narrates itself; cutting to budget
forces the reword. Tokens saved are context saved, both directions.

Budgets (non-blank lines and words, fenced code excluded from both):
    normal turn         6 lines / 150 words
    detail requested    18 lines / 400 words

"Detail requested" is keyed off the user's own words in the prompt that opened
the turn (explain, why, walk me through, in detail, full, design, plan, review).

Blocks at most twice per turn, then lets it through — a gate, not a trap.
Fails OPEN on any error: a broken gate never eats a turn.

Obsolete when: length stops being the symptom Peter flags, or the harness
grows a native output-budget control.
"""
import hashlib
import json
import re
import sys
from pathlib import Path

_STATE = Path(__file__).resolve().parent.parent / "telemetry" / "verbosity-gate-state.json"
_MAX_BLOCKS_PER_TURN = 2

_NORMAL = (6, 150)
_DETAIL = (18, 400)
_DETAIL_CUES = re.compile(
    r"\b(explain|why|walk me through|in detail|detailed|full(?:y)?|elaborate|"
    r"design|plan|review|compare|options|teach|how does)\b",
    re.I,
)

_FENCE = re.compile(r"^\s*```")


def _tail_messages(transcript_path):
    rows = []
    with open(transcript_path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return rows


def _text_of(row):
    """Concatenated text blocks of a transcript row; '' if none."""
    msg = row.get("message") or {}
    content = msg.get("content")
    if isinstance(content, str):
        return content
    if not isinstance(content, list):
        return ""
    return "\n".join(
        b.get("text", "") for b in content if isinstance(b, dict) and b.get("type") == "text"
    )


def _has_tool_use(row):
    content = (row.get("message") or {}).get("content")
    return isinstance(content, list) and any(
        isinstance(b, dict) and b.get("type") == "tool_use" for b in content
    )


def _measure(text):
    """(lines, words) outside fenced code blocks."""
    in_fence = False
    lines = 0
    words = 0
    for line in text.splitlines():
        if _FENCE.match(line):
            in_fence = not in_fence
            continue
        if in_fence or not line.strip():
            continue
        lines += 1
        words += len(line.split())
    return lines, words


def _block_count(session_id, turn_key):
    try:
        state = json.loads(_STATE.read_text(encoding="utf-8"))
    except Exception:
        state = {}
    entry = state.get(session_id) or {}
    return state, (entry.get("n", 0) if entry.get("key") == turn_key else 0)


def _record(state, session_id, turn_key, n):
    try:
        _STATE.parent.mkdir(parents=True, exist_ok=True)
        state[session_id] = {"key": turn_key, "n": n}
        _STATE.write_text(json.dumps(state), encoding="utf-8")
    except Exception:
        pass


def main():
    payload = json.load(sys.stdin)
    transcript = payload.get("transcript_path")
    if not transcript or not Path(transcript).exists():
        return 0

    rows = _tail_messages(transcript)

    last_assistant = next((r for r in reversed(rows) if r.get("type") == "assistant"), None)
    if last_assistant is None or _has_tool_use(last_assistant):
        return 0
    text = _text_of(last_assistant)
    if not text.strip():
        return 0

    last_user = next((r for r in reversed(rows) if r.get("type") == "user"), None)
    prompt = _text_of(last_user) if last_user else ""
    max_lines, max_words = _DETAIL if _DETAIL_CUES.search(prompt) else _NORMAL

    lines, words = _measure(text)
    if lines <= max_lines and words <= max_words:
        return 0

    session_id = payload.get("session_id", "?")
    # Stable across processes — PYTHONHASHSEED randomisation would reset the
    # block counter every invocation and turn the gate into a loop.
    turn_key = hashlib.md5(prompt.encode("utf-8", "replace")).hexdigest()
    state, blocked = _block_count(session_id, turn_key)
    if blocked >= _MAX_BLOCKS_PER_TURN:
        return 0
    _record(state, session_id, turn_key, blocked + 1)

    over = []
    if lines > max_lines:
        over.append(f"{lines} lines (budget {max_lines})")
    if words > max_words:
        over.append(f"{words} words (budget {max_words})")
    print(
        f"Over budget: {' and '.join(over)}. Condense — keep every fact, drop "
        "the padding. Reply with the shorter version only.",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:
        sys.exit(0)
