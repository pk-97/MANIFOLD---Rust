"""Shared windowing + classifier-call code for the substrate observer.

Used by both replay.py (offline harness) and observer.py (live daemon, built
in a later step) so the two never drift apart — DESIGN.md §4 requires replay
to "reuse the exact windowing the daemon will use." Nothing in here is
specific to historical replay; session-specific concerns (marker matching,
truncation, gate math) live in replay.py.
"""

import hashlib
import json
import os
import re
import subprocess
import tempfile
from datetime import datetime

CADENCE_EVENTS = 8
CADENCE_SECONDS = 90
MODEL = "claude-haiku-4-5-20251001"

# The classifier subprocess must never run with cwd inside this (or any)
# project: Claude Code auto-discovers CLAUDE.md + auto-memory from cwd
# regardless of --system-prompt/--setting-sources, which both pollutes the
# classifier with irrelevant project context and multiplies token cost
# roughly 3x (observed ~9.3k vs ~3.2k input tokens per call).
NEUTRAL_CWD = tempfile.gettempdir()

REJECTION_PREFIX = "The user doesn't want to proceed with this tool use"

TARGET_KEYS = ("file_path", "path", "notebook_path", "command", "pattern", "url", "query", "prompt")


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def strip_preamble(text):
    """Drop the authorship/template note before the first '---' separator."""
    parts = text.split("\n---\n", 1)
    return parts[1] if len(parts) == 2 else text


def parse_moves(text):
    """Parse moves.md into {move_id: {"signature": str, "cooldown": str, "payload": str}}."""
    moves = {}
    for block in re.split(r"(?m)^## ", text)[1:]:
        lines = block.splitlines()
        move_id = lines[0].strip()
        body = "\n".join(lines[1:])
        sig_m = re.search(r"-\s*\*\*signature:\*\*\s*(.*?)(?=\n-\s*\*\*cooldown:\*\*)", body, re.DOTALL)
        cd_m = re.search(r"-\s*\*\*cooldown:\*\*\s*(\S+)", body)
        pl_m = re.search(r"-\s*\*\*payload:\*\*\s*\n(.*)", body, re.DOTALL)
        signature = re.sub(r"\s+", " ", sig_m.group(1)).strip() if sig_m else ""
        cooldown = cd_m.group(1).strip() if cd_m else "standard"
        payload = pl_m.group(1).strip() if pl_m else ""
        # payload lines are blockquoted with "> " — strip that for the injected text
        payload = "\n".join(l[2:] if l.startswith("> ") else l for l in payload.splitlines()).strip()
        moves[move_id] = {"signature": signature, "cooldown": cooldown, "payload": payload}
    return moves


def validate_move_id(move_id, moves):
    """Returns move_id if it's a real, classifier-selectable move in the
    catalog, else None. `escalate/*` ids are daemon-selected only and are
    never valid coming from the classifier. An unrecognized id — e.g. the
    hallucinated `coaching/scope-drift` (nonexistent; only `anchor/scope-drift`
    is real) seen in replay round 2 — must be treated as clear, not silently
    accepted under a default cooldown class."""
    if not move_id or move_id not in moves or move_id.startswith("escalate/"):
        return None
    return move_id


def build_signature_catalog(moves):
    lines = []
    for mid in sorted(moves):
        if mid.startswith("escalate/"):
            continue  # daemon-selected, never offered to the classifier
        lines.append(f"### {mid}\n{moves[mid]['signature']}\n")
    return "\n".join(lines)


def build_system_prompt(rubric_text, moves):
    rubric_body = strip_preamble(rubric_text)
    return rubric_body.replace("{{SIGNATURES}}", build_signature_catalog(moves))


MODEL_TIER_NAMES = ("fable", "opus", "sonnet", "haiku")


def model_tier(model_id):
    """'claude-fable-5' / 'opus' / 'claude-opus-4-8' -> 'fable' / 'opus'.
    Returns None for unrecognized ids rather than guessing."""
    if not isinstance(model_id, str):
        return None
    low = model_id.lower()
    for tier in MODEL_TIER_NAMES:
        if tier in low:
            return tier
    return None


def tool_label(name, input_, session_model=None):
    """Ledger name for a tool call. Agent launches carry their model choice —
    `Agent[general-purpose@sonnet]` — because orchestrator model discipline
    (big model orchestrates, Sonnet executes) is judged from the ledger, and
    the prompt-only target line hides it. An omitted model param means the
    worker inherits the session's model, which is the silent way an Opus
    orchestrator spawns Opus workers — render it resolved (`@inherit:opus`)
    when the session model is known."""
    if name not in ("Agent", "Task") or not isinstance(input_, dict):
        return name
    explicit = model_tier(input_.get("model"))
    if explicit:
        model_str = explicit
    elif session_model:
        model_str = f"inherit:{session_model}"
    else:
        model_str = "inherit"
    agent_type = input_.get("subagent_type") or ""
    return f"{name}[{agent_type}@{model_str}]" if agent_type else f"{name}[@{model_str}]"


def tool_target(input_):
    if not isinstance(input_, dict):
        return ""
    for key in TARGET_KEYS:
        v = input_.get(key)
        if isinstance(v, str):
            return _truncate(v)
    for v in input_.values():
        if isinstance(v, str):
            return _truncate(v)
    return ""


def _truncate(s, n=100):
    s = s.replace("\n", " ")
    return s if len(s) <= n else s[: n - 1] + "…"


def _flatten_content_text(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(c.get("text", "") for c in content if isinstance(c, dict) and c.get("type") == "text")
    return ""


def tool_result_status(block):
    if block.get("is_error"):
        return "err"
    if _flatten_content_text(block.get("content")).startswith(REJECTION_PREFIX):
        return "err"
    return "ok"


TASK_ADDRESSED_MIN_CHARS = 40  # a trivial ack ("OK.", "Got it.") doesn't count as addressing TASK


def format_window(task, ledger, recent, task_addressed=False):
    task_str = task if task else "(no task statement yet)"
    if task and task_addressed:
        task_str += " — already addressed by a prior reply this session"
    ledger_str = " · ".join(ledger) if ledger else "(no tool events)"
    recent_str = "\n---\n".join(t.strip() for t in recent) if recent else "(no assistant text yet)"
    return f'TASK: "{task_str}"\n\nLEDGER: `{ledger_str}`\n\nRECENT:\n{recent_str}'


def parse_ts(ts_raw):
    if not ts_raw:
        return None
    try:
        return datetime.fromisoformat(ts_raw.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


class WindowState:
    """Stateful windowing — the exact per-event state machine the live daemon
    uses. Feed assistant content on assistant turns, user content (with a
    timestamp) on user turns; a closed window is returned the moment cadence
    triggers. The caller owns session-specific concerns (marker matching,
    truncation) and only sees closed windows + raw human text.
    """

    def __init__(self):
        self.current_task = None
        self.recent_texts = []
        self.ledger_buffer = []
        self.tool_event_count_since_window = 0
        self.total_tool_event_count = 0
        self.last_window_ts = None
        self.pending = {}
        self.task_addressed = False
        self.session_model = None  # tier of the last assistant event's model id

    def _close_window(self, ts):
        closed = {
            "end_event_count": self.total_tool_event_count,
            "end_ts": ts,
            "text": format_window(self.current_task, self.ledger_buffer, self.recent_texts, self.task_addressed),
        }
        self.ledger_buffer = []
        self.tool_event_count_since_window = 0
        self.last_window_ts = ts
        return closed

    def feed_assistant_content(self, content, ts=None, model=None):
        """Feed one assistant turn's content blocks. Returns a closed window
        the moment a text block lands (with a TASK already set) — drift
        markers ARE assistant texts, and waiting for the next cadence tick
        misses the 1-3 tool events that landed right after the last window
        closed (the dominant cadence-miss cluster in replay round 2: altitude,
        enumerate-levels, hedge-creep, destructive-isolation). Returns None
        when no text block was seen, or none has been seen since the task was
        set (nothing to judge against yet)."""
        closed = None
        tier = model_tier(model)
        if tier:
            self.session_model = tier
        for c in content:
            if not isinstance(c, dict):
                continue
            if c.get("type") == "text":
                text = c.get("text", "")
                stripped = text.strip()
                if stripped:
                    self.recent_texts.append(text)
                    self.recent_texts[:] = self.recent_texts[-2:]
                    if self.current_task is not None:
                        if len(stripped) >= TASK_ADDRESSED_MIN_CHARS:
                            self.task_addressed = True
                        closed = self._close_window(ts if ts is not None else self.last_window_ts)
            elif c.get("type") == "tool_use":
                self.pending[c.get("id")] = (c.get("name", "?"), c.get("input", {}) or {})
        return closed

    def feed_user_content(self, content, ts):
        """Returns (closed_window_or_None, list_of_human_text_seen).
        `content` is a raw string for a plain typed message with no
        attachments, or a list of content blocks otherwise — both occur
        routinely in real transcripts."""
        closed = None
        human_texts = []
        if isinstance(content, str):
            content = [{"type": "text", "text": content}]
        for c in content:
            if not isinstance(c, dict):
                continue
            ctype = c.get("type")
            if ctype == "tool_result":
                name, input_ = self.pending.pop(c.get("tool_use_id"), ("?", {}))
                label = tool_label(name, input_, self.session_model)
                self.ledger_buffer.append(f"{label} {tool_target(input_)} {tool_result_status(c)}".strip())
                self.tool_event_count_since_window += 1
                self.total_tool_event_count += 1

                fire = self.tool_event_count_since_window >= CADENCE_EVENTS
                if not fire and self.last_window_ts is not None and ts is not None:
                    fire = (ts - self.last_window_ts) >= CADENCE_SECONDS
                if fire and self.ledger_buffer:
                    closed = self._close_window(ts)
            elif ctype == "text":
                text = c.get("text", "")
                stripped = text.strip()
                if stripped:
                    human_texts.append(stripped)
                    if len(stripped) >= 8:
                        self.current_task = stripped
                        self.task_addressed = False
        return closed, human_texts


def extract_json(text):
    text = text.strip()
    m = re.match(r"^```(?:json)?\s*(.*?)\s*```$", text, re.DOTALL)
    if m:
        text = m.group(1).strip()
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        pass
    start, end = text.find("{"), text.rfind("}")
    if start != -1 and end != -1 and end > start:
        try:
            return json.loads(text[start : end + 1])
        except json.JSONDecodeError:
            return None
    return None


def call_classifier(system_prompt, window_text, model=MODEL, timeout=60, cwd=None):
    """Invoke `claude -p` as the classifier. Same invocation shape the live
    daemon uses. Returns a verdict dict, or {"error": "..."} on any failure —
    callers must treat an error verdict as "no flag" (fail open).

    `cwd` defaults to NEUTRAL_CWD (never the project) — see the module-level
    comment. Pass an explicit `cwd` only for tests that want to observe that
    behavior."""
    cmd = [
        "claude", "-p",
        "--model", model,
        "--system-prompt", system_prompt,
        "--tools", "",
        "--setting-sources", "",
        "--output-format", "json",
        "--no-session-persistence",
        window_text,
    ]
    try:
        proc = subprocess.run(
            cmd, capture_output=True, text=True, timeout=timeout, cwd=cwd or NEUTRAL_CWD
        )
    except subprocess.TimeoutExpired:
        return {"error": "timeout"}
    except OSError as e:
        return {"error": f"spawn failed: {e}"}
    if proc.returncode != 0:
        # error detail can land on either stream; stderr is often empty
        detail = (proc.stderr.strip() or proc.stdout.strip())[:200]
        return {"error": f"exit {proc.returncode}: {detail}"}
    try:
        outer = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return {"error": "bad outer json", "raw": proc.stdout[:200]}
    if outer.get("is_error"):
        return {"error": f"api error: {outer.get('result', '')[:200]}"}
    verdict = extract_json(outer.get("result", ""))
    if verdict is None:
        return {"error": "unparseable verdict", "raw": outer.get("result", "")[:200]}
    return verdict


class VerdictCache:
    """Content-addressed verdict cache: key = hash(system_prompt + window text),
    so identical questions are never paid for twice, and any rubric/signature
    edit automatically invalidates (the prompt is part of the key). Error
    verdicts are never cached — they represent a failed call, not an answer.
    Storage is a JSONL file, append-on-put, loaded once."""

    def __init__(self, path):
        self.path = path
        self.mem = {}
        if os.path.exists(path):
            with open(path, encoding="utf-8") as f:
                for line in f:
                    try:
                        d = json.loads(line)
                        self.mem[d["k"]] = d["v"]
                    except (json.JSONDecodeError, KeyError):
                        continue

    @staticmethod
    def key(system_prompt, window_text):
        return hashlib.sha256((system_prompt + "\x00" + window_text).encode("utf-8")).hexdigest()

    def get(self, system_prompt, window_text):
        return self.mem.get(self.key(system_prompt, window_text))

    def put(self, system_prompt, window_text, verdict):
        if "error" in (verdict or {"error": True}):
            return
        k = self.key(system_prompt, window_text)
        if k in self.mem:
            return
        self.mem[k] = verdict
        with open(self.path, "a", encoding="utf-8") as f:
            f.write(json.dumps({"k": k, "v": verdict}) + "\n")


COOLDOWN_EVENTS = {"standard": 20, "slow": 40}  # "once" handled separately (fire-at-most-once)


def apply_cooldowns(fires, move_cooldowns):
    """fires: list of (event_count, move_id, window) in chronological order.
    Returns the subset that would actually reach the model after daemon-side
    cooldown suppression (DESIGN.md §1)."""
    last_fire_event = {}
    effective = []
    for event_count, move_id, window in fires:
        cd_class = move_cooldowns.get(move_id, "standard")
        if cd_class == "once":
            if move_id in last_fire_event:
                continue
        else:
            limit = COOLDOWN_EVENTS.get(cd_class, 20)
            prev = last_fire_event.get(move_id)
            if prev is not None and (event_count - prev) < limit:
                continue
        last_fire_event[move_id] = event_count
        effective.append((event_count, move_id, window))
    return effective
