#!/usr/bin/env python3
"""Stop hook: the daemon's turn-end valve (DESIGN.md §2 "Known delivery gap",
RULED 2026-07-04, spec confirmed against the current hooks reference).

A flag raised on a turn's FINAL assistant text has no delivery channel until
the next human prompt — PostToolUse never fires again that turn, and that is
exactly where verify-claim's most common firing position (done-claims are
turn-final) lands one turn late. This hook may block the Stop event ONCE,
delivering a pending whisper as the block reason so the model gets one beat
to self-correct before yielding.

Re-ruled 2026-07-05 (Peter): the classifier race is no longer accepted —
turn-final corrections landing on the NEXT prompt defeated the point. The
hook now waits (bounded) for the observer to CATCH UP: the observer
publishes a `.offset` heartbeat after each drain, classification runs
synchronously inside the drain, so heartbeat >= transcript-size-at-Stop
means every window this turn produced has been judged and any verdict is
on disk. The wait is capped (STOP_WAIT_CAP_S), skipped entirely when the
observer is dead or pre-heartbeat (fail open), and delivers the instant a
verdict appears. Typical cost is one observer poll (~1s); the cap only
binds when a classification is genuinely in flight — exactly the turns
the whisper is for.

A second, purely deterministic check (never the classifier — moves.md's
mechanical/announced-not-started) catches a turn whose final text announces
imminent action ("Starting X now", "Beginning X", "Let me now...") with no
tool call following it in the turn.

Guard: "block at most once per turn" is enforced two ways — a
`<session>.stopblock.<prompt_id>` sentinel file (the durable guard; a turn is
one prompt_id, regardless of which mailbox supplied the whisper) and the
`stop_hook_active` stdin field if the running Claude Code version sets it
(undocumented in the current hooks reference as of 2026-07-04, but free
defense-in-depth if present). Either one present means this turn already
spent its one block.

Fails open on every error; never raises, never leaves the process non-zero.
"""
import json
import os
import re
import sys
import time

HOOKS_DIR = os.path.dirname(os.path.abspath(__file__))
DAEMON_DIR = os.path.normpath(os.path.join(HOOKS_DIR, "..", "daemon"))
sys.path.insert(0, DAEMON_DIR)
# Same derivation style as DAEMON_DIR above, one level further up: .claude/hooks
# -> .claude -> repo root. Used by mechanical/ungrounded-chat-claim (DESIGN.md
# §2h.1) to resolve ALL-CAPS doc-token candidates against docs/<token>.md.
REPO_ROOT = os.path.normpath(os.path.join(HOOKS_DIR, "..", ".."))

STOPBLOCK_MAX_AGE = 24 * 60 * 60  # sentinels older than a day are stale, sweep them

# Catch-up wait bounds (DESIGN.md §2 re-ruling + §2h.6 forensics, 2026-07-07).
# STOP_WAIT_CAP_S was 6.0 until the forensics run: all 11 TEXT-ONLY-RACE late
# fires ran to EXACTLY the 6.0s cap (durations clustered 6130-6250ms), meaning
# the classifier was close behind and the cap — not observer death — was the
# binding constraint. §2h.6(b) re-prices that trade per Peter's 2026-07-07
# metric ruling ("the daemon must fire BEFORE my next message — otherwise
# it's pointless"). Throttling degradation unchanged: dead/stale observer
# still means no wait at all.
STOP_WAIT_CAP_S = 10.0
# §2h.6(a): gap between the transcript stats at Stop entry — see
# _stable_transcript_size below.
STOP_STAT_GAP_S = 0.2

# Grade-backstop (2026-07-05 review): self-grades can't be joined to fires
# when sessions never write them at all. Deterministic, mirrors the
# announced-not-started check below — main-session only, fires at most ONCE
# per session (its own sentinel, not the per-turn stopblock, since nagging
# every turn until the backlog clears would defeat "before the session
# ends"). No moves.md entry: this is valve plumbing, not a drift move, so
# the reminder text is authored directly here.
GRADEABLE_MOVE_PREFIXES = ("anchor/", "coaching/", "escalate/")
GRADE_BACKSTOP_STALE_EVENTS = 40  # matches §2d's oscillation-span convention
GRADE_BACKSTOP_MOVE_ID = "mechanical/grade-backstop"

# DESIGN.md §2h.4: workers run far shorter than the main session, so the
# main-session gates above (GRADE_BACKSTOP_STALE_EVENTS / OBSERVATION_PROMPT_
# MIN_EVENTS below, both 40) would exempt nearly every real worker from ever
# tripping either one. One lower constant covers both roles for a worker
# mailbox: how stale an ungraded fire must be before the grade backstop nags,
# and how much activity a worker needs before the observation prompt is worth
# asking at all.
WORKER_ACTIVITY_MIN_EVENTS = 20

# Observation review prompt (2026-07-05, Peter's ask): a standing invitation
# to log anything worth the next sleep pass's attention, asked at most ONCE
# per session (own sentinel below, mirroring the grade backstop) — most
# sessions have nothing to add, and asking every turn until something gets
# written would force busywork just to go quiet. Also gated on a minimum
# amount of session activity (same "has this session had enough happen yet"
# convention as the grade backstop / §2d) — a session a couple of tool calls
# long hasn't had a "session" worth reviewing. No moves.md entry (valve
# plumbing, not a drift move).
OBSERVATION_PROMPT_MOVE_ID = "mechanical/observation-prompt"
OBSERVATION_PROMPT_MIN_EVENTS = 40

# Conservative imminent-action triggers (moves.md mechanical/announced-not-started).
# Matched against the LAST sentence of the last assistant text only. A
# trailing "?" always disqualifies first — a question to the user is never
# this signature, whatever it starts with. Future-conditional phrasing
# ("I'll do X once you confirm") never starts with any of these, so it's
# excluded by construction rather than by an extra negative check.
_STARTING_NOW_RE = re.compile(r"^(?:starting|doing)\b.*?\bnow\b", re.IGNORECASE | re.DOTALL)
_BEGINNING_RE = re.compile(r"^beginning\b", re.IGNORECASE)
_LET_ME_NOW_RE = re.compile(r"^let me now\b", re.IGNORECASE)


def _last_sentence(text):
    text = (text or "").strip()
    if not text:
        return ""
    parts = [p.strip() for p in re.split(r"(?<=[.!?])\s+|\n+", text) if p.strip()]
    return parts[-1] if parts else ""


def _is_announcement(sentence):
    if not sentence or sentence.endswith("?"):
        return False
    return bool(
        _STARTING_NOW_RE.match(sentence)
        or _BEGINNING_RE.match(sentence)
        or _LET_ME_NOW_RE.match(sentence)
    )



# ---- mechanical/ungrounded-chat-claim (DESIGN.md §2h.1) ----
#
# Deterministic, valve-selected at Stop time — never the classifier: chat
# turns are exactly where the classifier's Stop catch-up wait has nothing to
# catch (zero tool events means no window ever closes for it to judge). The
# moves.md signature is the contract this block implements verbatim.

_FENCE_RE = re.compile(r"```.*?```", re.DOTALL)

# Known repo roots and code/doc extensions an assertion can name. Purely
# syntactic — no existence check for these two forms (moves.md's signature
# only requires resolution for the ALL-CAPS form below); grounding is the
# actual safety net, checked separately against earlier tool-call INPUTS.
_ARTIFACT_ROOTS = ("docs/", "crates/", "src/", "assets/", "scripts/", ".claude/")
_ARTIFACT_EXTENSIONS = ("rs", "md", "py", "json", "wgsl", "toml")
_SLASH_ARTIFACT_RE = re.compile(
    "(?:" + "|".join(re.escape(r) for r in _ARTIFACT_ROOTS) + r")[\w./-]*[\w]"
)
# Requires a "/" in the match (T3, 2026-07-07 fix): this form exists to catch
# extension-having sub-paths not anchored to a known top root (e.g. a nested
# path mentioned mid-crate, "node_graph/primitives/foo.rs", without the
# "crates/" prefix) — a BARE filename with no slash at all ("moves.md") was
# never this pattern's intent, but the unconstrained regex swept those in too
# with no existence check, so a plausible-but-nonexistent bare filename could
# already fire. Bare filenames now go exclusively through the gated
# _BARE_FILENAME_RE below (stat-checked), keeping this form's "no existence
# check, grounding is the safety net" contract scoped to genuine paths.
_EXT_ARTIFACT_RE = re.compile(r"[\w][\w./-]*/[\w.-]*\.(?:" + "|".join(_ARTIFACT_EXTENSIONS) + r")\b")

# ALL-CAPS underscore-joined tokens: only a candidate, membership requires
# os.path.exists(docs/<token>.md) against the repo root (below) — the regex
# alone is not enough per moves.md's signature.
_ALLCAPS_ARTIFACT_RE = re.compile(r"\b[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+\b")

# TICKETS.md T3(a): bare relative filenames (no slash, no ALL-CAPS) with a
# code/doc extension — e.g. "moves.md now documents..." — a near-miss
# (ef0c8e89) asserted moves.md's contents and matched none of the existing
# forms. Only a candidate: membership requires a stat-check against the repo
# root or .claude/daemon/ (below), same discipline as the ALL-CAPS form, so a
# generic word like "notes.md" that doesn't exist never fires.
_BARE_FILENAME_RE = re.compile(r"\b[\w][\w.-]*\.(?:" + "|".join(_ARTIFACT_EXTENSIONS) + r")\b")

# TICKETS.md T3(b): move-id-shaped tokens (`family/kebab-name`), e.g.
# "mechanical/confessed-stopgap already covers this" — only a candidate,
# membership requires resolving against moves.md's own `## family/kebab-name`
# headings (below), so a plausible-looking but fake token never fires.
_MOVE_ID_TOKEN_RE = re.compile(r"\b[a-z][a-z-]*/[a-z][a-z-]*\b")

# Self-marked recall/proposal text: skip the whole detection, not just the
# named artifact — moves.md: "the text does not mark itself as recall or
# proposal".
_RECALL_MARKER_RE = re.compile(
    r"\bi think\b|\bfrom memory\b|\bif i recall\b|\bprobably\b|\bproposal\b|"
    r"\bnot checked\b|\bunverified\b",
    re.IGNORECASE,
)


def _moves_md_ids():
    """Real move ids from moves.md's own `## family/kebab-name` headings,
    for TICKETS.md T3(b)'s move-id-shaped-token recognizer. Reuses
    common.parse_moves rather than re-deriving the heading format. Never
    raises — returns an empty set on any failure."""
    try:
        import common

        moves = common.parse_moves(common.read(os.path.join(DAEMON_DIR, "moves.md")))
        return set(moves.keys())
    except Exception:
        return set()


def _extract_chat_artifacts(text, repo_root):
    """Candidate repo artifacts named in `text`, fenced code blocks stripped
    first (deliverables and quoted prompts inside a fence are cargo, not a
    claim about repo state)."""
    stripped = _FENCE_RE.sub(" ", text or "")
    found = set()
    for pattern in (_SLASH_ARTIFACT_RE, _EXT_ARTIFACT_RE):
        found.update(m.group(0) for m in pattern.finditer(stripped))
    for m in _ALLCAPS_ARTIFACT_RE.finditer(stripped):
        token = m.group(0)
        if os.path.exists(os.path.join(repo_root, "docs", f"{token}.md")):
            found.add(token)
    for m in _BARE_FILENAME_RE.finditer(stripped):
        name = m.group(0)
        if os.path.exists(os.path.join(repo_root, name)) or os.path.exists(os.path.join(DAEMON_DIR, name)):
            found.add(name)
    for m in _MOVE_ID_TOKEN_RE.finditer(stripped):
        token = m.group(0)
        if token in _moves_md_ids():
            found.add(token)
    return found


def _human_prompt_text(content):
    """Text of a genuine human chat message, or None if this 'user' JSONL
    entry is actually a tool_result carrier (harness-generated in response to
    a tool call, not a new human turn)."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        texts = []
        for block in content:
            if not isinstance(block, dict):
                continue
            if block.get("type") == "tool_result":
                return None
            if block.get("type") == "text":
                texts.append(block.get("text", ""))
        return "\n".join(texts) if texts else None
    return None


def _parse_last_turn(transcript_path):
    """Parse the transcript's LAST turn: from the final genuine human prompt
    (a 'user' entry that isn't a tool_result carrier) to the end of the
    file. This is turn-WIDE — every assistant JSONL entry after the
    boundary, not just the final one — unlike mechanical/announced-not-
    started's check, which only looks at the last assistant message's own
    trailing content. Returns (final_text, user_prompt_text, tool_calls)
    where tool_calls is [(name, input_dict)] in call order. Shared by the
    §2h.1 chat-claim and §2h.6(c) done-claim checks — same turn boundary,
    opposite requirements on tool_calls (zero vs. mutating-without-
    verification). (None, None, []) when there's no usable boundary or no
    assistant text after it."""
    entries = []
    try:
        with open(transcript_path, encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    entries.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
    except OSError:
        return None, None, []

    boundary = None
    user_prompt_text = ""
    for i in range(len(entries) - 1, -1, -1):
        d = entries[i]
        if d.get("type") != "user":
            continue
        text = _human_prompt_text(d.get("message", {}).get("content"))
        if text is not None:
            boundary, user_prompt_text = i, text
            break
    if boundary is None:
        return None, None, []

    final_text = None
    tool_calls = []
    for d in entries[boundary + 1 :]:
        if d.get("type") != "assistant":
            continue
        content = d.get("message", {}).get("content")
        if not isinstance(content, list):
            continue
        for block in content:
            if not isinstance(block, dict):
                continue
            if block.get("type") == "tool_use":
                input_ = block.get("input")
                tool_calls.append((block.get("name", "?"), input_ if isinstance(input_, dict) else {}))
            elif block.get("type") == "text":
                t = block.get("text", "")
                if t.strip():
                    final_text = t
    return final_text, user_prompt_text, tool_calls


def _last_turn_zero_tool_calls(transcript_path):
    """mechanical/ungrounded-chat-claim's turn gate: moves.md's signature is
    "the turn contains zero tool calls" — any tool call anywhere in the turn
    disqualifies. Returns (final_text, user_prompt_text), final_text None
    when disqualified."""
    final_text, user_prompt_text, tool_calls = _parse_last_turn(transcript_path)
    if tool_calls:
        return None, None
    return final_text, user_prompt_text


def _flatten_strings_into(value, out):
    if isinstance(value, str):
        out.append(value)
    elif isinstance(value, dict):
        for v in value.values():
            _flatten_strings_into(v, out)
    elif isinstance(value, list):
        for v in value:
            _flatten_strings_into(v, out)


def _collect_tool_input_strings(transcript_path):
    """Every string value nested anywhere inside a tool_use call's `input`,
    across the WHOLE transcript, joined into one blob — Read/Edit/Write/Grep/
    Glob/LSP file arguments, Bash command strings, and so on (catchup counts:
    this is a plain linear scan, not bounded to live-tailed events). Tool
    OUTPUTS (tool_result content) are never read here — a mention inside a
    read's output is the stale-memory failure this move exists to catch, not
    provenance for it."""
    parts = []
    try:
        with open(transcript_path, encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    d = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if d.get("type") != "assistant":
                    continue
                content = d.get("message", {}).get("content")
                if not isinstance(content, list):
                    continue
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "tool_use":
                        _flatten_strings_into(block.get("input"), parts)
    except OSError:
        pass
    return "\n".join(parts)


def _ungrounded_chat_claim(transcript_path, repo_root=REPO_ROOT):
    """Returns the (sorted-first) ungrounded artifact name if
    mechanical/ungrounded-chat-claim's signature holds for the turn at the
    end of `transcript_path`, else None. Never raises (caught by main()'s
    top-level try/except, same fail-open contract as every other check
    here)."""
    final_text, user_prompt_text = _last_turn_zero_tool_calls(transcript_path)
    if not final_text:
        return None
    if _RECALL_MARKER_RE.search(final_text):
        return None
    artifacts = _extract_chat_artifacts(final_text, repo_root)
    if not artifacts:
        return None
    artifacts = {a for a in artifacts if a not in (user_prompt_text or "")}
    if not artifacts:
        return None
    grounded_blob = _collect_tool_input_strings(transcript_path)
    ungrounded = sorted(a for a in artifacts if a not in grounded_blob)
    return ungrounded[0] if ungrounded else None


def _stable_transcript_size(transcript_path):
    """DESIGN.md §2h.6(a) — the snapshot-race fix. The Stop event can race
    the harness's write of the turn-final text: 5 of the 17 late fires in the
    2026-07-07 forensics were VERDICT-AFTER-TURN (hook durationMs 27-44 with
    the flagged text landing 27-77ms BEFORE Stop) — a single stat at Stop
    entry read a stale size, the observer's heartbeat already covered it, and
    the hook concluded caught-up and skipped the wait with the observer
    provably alive. Stat twice ~200ms apart and take the max; if the file
    grew between the two stats it may still be mid-flush, so re-stat once
    more after another gap (bounded at three stats total — this is a race
    window measured in tens of ms, not a tail we chase forever). Returns
    None when the first stat fails (caller skips the wait, fail open); a
    later stat failing falls back to the best size seen so far."""
    try:
        first = os.path.getsize(transcript_path)
    except OSError:
        return None
    time.sleep(STOP_STAT_GAP_S)
    try:
        second = os.path.getsize(transcript_path)
    except OSError:
        return first
    if second <= first:
        return first  # stable across the gap: the common case, one gap paid
    time.sleep(STOP_STAT_GAP_S)
    try:
        third = os.path.getsize(transcript_path)
    except OSError:
        return second
    return max(second, third)


# DESIGN.md §2 Stop-wait CONVERT fix (sleep pass 2, PASS2_AGENDA item 1).
# The catch-up wait as first built waited passively for the observer's
# `.offset` heartbeat to reach the transcript size at Stop. The 2026-07-07
# durationMs forensics found it CONVERTED nothing: of 114 post-fix Stop
# invocations the wait either returned <0.1s (a verdict already on disk at Stop
# entry — the step-1 fast path) or ran the full ~10.5s cap; ZERO landed in
# between, i.e. no wait ever delivered a turn-final verdict it didn't already
# have. Root cause: the observer classifies the turn-final window on its own
# 1s poll cadence, often behind a backlog of earlier windows in the same
# synchronous drain, and only publishes its offset (all-or-nothing) after that
# whole drain finishes — so within the cap the wait sees neither the offset
# advance nor a verdict. The poke below asks the observer to classify the
# turn-final window FIRST (see observer.py _drain priority path) and to remove
# this file once it has classified through `target`; the wait then delivers any
# verdict (mailbox checked first) or, on a no-drift turn, breaks the instant the
# poke clears instead of burning the cap. Best-effort on both sides; a
# dead/stalled observer never clears it and the wait still caps out, fail-open.
def _write_poke(poke_path, target):
    try:
        os.makedirs(os.path.dirname(poke_path), exist_ok=True)
        tmp = f"{poke_path}.tmp.{os.getpid()}"
        with open(tmp, "w", encoding="utf-8") as f:
            f.write(str(target))
        os.replace(tmp, poke_path)
    except OSError:
        pass


def _clear_poke(poke_path):
    try:
        os.remove(poke_path)
    except OSError:
        pass


# ---- mechanical/unverified-done-claim (DESIGN.md §2h.6(c)) ----
#
# The zero-latency tier for the anchor/verify-claim family: the 2026-07-07
# forensics showed 8 of the 11 classifier-latency late fires were done-claim
# family, and a completion claim is structurally the last text of its turn,
# so it always races the Stop wait. Deterministic and crude by design — the
# classifier keeps the nuanced cases (bundled claims, wrong-medium evidence);
# this catches the literal form instantly. The moves.md signature is the
# contract.

# First-person completion claims, matched per sentence as leading words
# (which subsumes standalone claim sentences — "Done." starts with "done");
# an optional "all/everything" prefix covers "All done.".
_DONE_CLAIM_RE = re.compile(
    r"^(?:(?:all|everything)\s+)?"
    r"(?:done|fixed|landed|shipped|pushed|implemented|complete|resolved|works now)\b",
    re.IGNORECASE,
)
# The final text already confessing unverified-ness skips the fire — the
# payload's own escape hatch ("end with 'unverified' instead of 'done'")
# must not itself re-trigger the move next turn.
_DONE_CONFESSION_RE = re.compile(
    r"\bunverified\b|\bnot verified\b|\bhaven'?t run\b|\bstill needs\b|\bowed\b|\buntested\b",
    re.IGNORECASE,
)
# Read-only Bash commands (first command position only — crude tier, same
# spirit as common.py's regex tables): these never count as mutating work.
_READONLY_BASH_RE = re.compile(
    r"^\s*(?:rg|grep|fd|find|ls|cat|head|tail|wc|jq|sort|uniq|diff|stat|file|which|pwd|du|df|echo|tree)\b"
)
# Git commands are exempted wholesale per the signature: a commit/push-only
# turn's claim is usually about the git action itself, which its own success
# output verifies.
_GIT_BASH_RE = re.compile(r"^\s*git\b")


def _contains_done_claim(text):
    sentences = [p.strip() for p in re.split(r"(?<=[.!?])\s+|\n+", text or "") if p.strip()]
    # lstrip markdown decoration so "**Done.**" still reads as leading-word
    # "done" — decoration is emphasis, not a different sentence.
    return any(_DONE_CLAIM_RE.match(s.lstrip("*_#>`- ")) for s in sentences)


def _unverified_done_claim(transcript_path):
    """True iff mechanical/unverified-done-claim's ALL-hold signature holds
    for the turn at the end of `transcript_path`: a completion claim in the
    final text, at least one non-git mutating tool event in the turn, zero
    verification-class events (common.py's detect_verification_class table),
    and no confession in the final text. Never raises (caught by main()'s
    top-level try/except)."""
    final_text, _user_prompt, tool_calls = _parse_last_turn(transcript_path)
    if not final_text or not tool_calls:
        # No-tool turns belong to ungrounded-chat-claim / the classifier —
        # "retrospective chat mentions of past completions" per the signature.
        return False
    if not _contains_done_claim(final_text):
        return False
    if _DONE_CONFESSION_RE.search(final_text):
        return False
    import common

    has_non_git_mutation = False
    for name, input_ in tool_calls:
        if common.detect_verification_class(name, input_):
            return False  # the turn verified something: not this signature
        if name in ("Edit", "Write", "MultiEdit"):
            has_non_git_mutation = True
        elif name == "Bash":
            cmd = input_.get("command") or ""
            if _GIT_BASH_RE.match(cmd) or _READONLY_BASH_RE.match(cmd):
                continue
            has_non_git_mutation = True
    return has_non_git_mutation


def _last_assistant_content(transcript_path):
    """One linear pass over the transcript, returning the LAST assistant
    message's content list (or None). Tolerant of malformed lines, matching
    observer.py's own transcript-reading style. This runs once per Stop
    event, not on a hot path, so a full scan is the simplest correct thing —
    no tail-seeking required."""
    last_content = None
    with open(transcript_path, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                d = json.loads(line)
            except json.JSONDecodeError:
                continue
            if d.get("type") != "assistant":
                continue
            content = d.get("message", {}).get("content")
            if isinstance(content, list):
                last_content = content
    return last_content


def _announced_not_started(transcript_path):
    content = _last_assistant_content(transcript_path)
    if not content:
        return False
    last_text_idx, last_text, tool_use_after = None, None, False
    for i, block in enumerate(content):
        if not isinstance(block, dict):
            continue
        btype = block.get("type")
        if btype == "text":
            last_text_idx, last_text = i, block.get("text", "")
        elif btype == "tool_use" and last_text_idx is not None and i > last_text_idx:
            tool_use_after = True
    if last_text is None or tool_use_after:
        return False
    return _is_announcement(_last_sentence(last_text))


def _sweep_stale_sentinels(verdicts_dir):
    try:
        now = time.time()
        for name in os.listdir(verdicts_dir):
            if ".stopblock." not in name:
                continue
            path = os.path.join(verdicts_dir, name)
            try:
                if now - os.path.getmtime(path) > STOPBLOCK_MAX_AGE:
                    os.remove(path)
            except OSError:
                pass
    except OSError:
        pass


def _session_gradeable_fires(telemetry_path, session_id, agent_id=None):
    """(seq, move_id, ts) for every gradeable (anchor/coaching/escalate) fire
    delivered to THIS mailbox — the session's own (agent_id=None, same scope
    as §4b's scoring) or, per DESIGN.md §2h.4, one worker's own mailbox
    (agent_id set) — oldest first. Reads telemetry.jsonl directly rather than
    needing a new field; never raises."""
    fires = []
    try:
        with open(telemetry_path, encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if rec.get("event") != "injected" or rec.get("agent_id") != agent_id:
                    continue
                if rec.get("session_id") != session_id:
                    continue
                move_id = rec.get("move_id") or ""
                if not move_id.startswith(GRADEABLE_MOVE_PREFIXES):
                    continue
                if rec.get("seq") is None:
                    continue
                fires.append((rec["seq"], move_id, rec.get("ts")))
    except OSError:
        return []
    fires.sort(key=lambda t: t[0])
    return fires


def _session_grade_count(eval_dir, session_id, agent_id=None):
    """How many grade lines (any file matching live_grades*.jsonl) this
    mailbox — session-level (agent_id=None) or one worker's own (agent_id
    set, DESIGN.md §2h.4) — has already written. A coarse backstop count, not
    the precise per-fire join slice_fires.py does for the sleep pass. Records
    with no "agent_id" key at all read as agent_id=None via .get, matching
    every pre-§2h.4 session self-grade line on disk."""
    import glob

    count = 0
    for path in glob.glob(os.path.join(eval_dir, "live_grades*.jsonl")):
        try:
            with open(path, encoding="utf-8", errors="replace") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        rec = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if rec.get("session_id") != session_id:
                        continue
                    if rec.get("agent_id") != agent_id:
                        continue
                    count += 1
        except OSError:
            continue
    return count


def _events_since(transcript_path, since_ts):
    """Count tool_result blocks (the same 'event' unit common.py's
    WindowState counts — one completed tool call) whose containing message
    postdates `since_ts`. Reads the transcript directly so this needs no new
    telemetry field."""
    if since_ts is None:
        return 0
    import common

    count = 0
    try:
        with open(transcript_path, encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    d = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if d.get("type") != "user":
                    continue
                ts = common.parse_ts(d.get("timestamp"))
                if ts is not None and ts <= since_ts:
                    continue
                content = (d.get("message") or {}).get("content")
                if not isinstance(content, list):
                    continue
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "tool_result":
                        count += 1
    except OSError:
        return count
    return count


def _grade_backstop_reason(ungraded_count, oldest_events_ago, agent_id=None):
    # DESIGN.md §2h.4: a worker's grade lines need "agent_id" too — RUNBOOK.md
    # step 2: "(session_id, seq) alone collides across workers". Only this
    # clause varies with the mailbox; the rest of the sentence is frozen
    # (same invariant-5 precedent as the seq numeral elsewhere).
    agent_note = (
        f' Since this fire belongs to a worker, also include "agent_id": '
        f'"{agent_id}" on its line — (session_id, seq) alone collides across '
        f"workers."
        if agent_id
        else ""
    )
    return (
        f'<daemon move="{GRADE_BACKSTOP_MOVE_ID}">\n'
        f"This session delivered {ungraded_count} gradeable daemon fire(s) "
        f"(anchor/coaching/escalate) with no self-grade recorded yet, and the "
        f"oldest is {oldest_events_ago} tool events old. Before the session "
        f"ends, append one self-grade line per ungraded fire to "
        f".claude/daemon/eval/live_grades.session.jsonl — canonical "
        f"correct/effective values and format in RUNBOOK.md step 2, and "
        f'include each fire\'s own "seq" so the sleep pass can join the grade '
        f"back to the exact fire it belongs to.{agent_note}\n"
        f"</daemon>"
    )


def _observation_prompt_reason(agent_id=None):
    agent_note = f', "agent_id": "{agent_id}"' if agent_id else ""
    return (
        f'<daemon move="{OBSERVATION_PROMPT_MOVE_ID}">\n'
        f"Before this session goes further: is there anything worth logging "
        f"for the daemon — a drift the observer should have caught but "
        f"didn't, a pattern that doesn't fit an existing move, or any other "
        f"note for the next sleep pass? Most sessions won't have anything, "
        f"and that's fine — no action needed either way. If something IS "
        f"worth logging, append one record to "
        f".claude/daemon/eval/observations.session.jsonl: {{ts, session_id, "
        f'kind: "miss-candidate"|"note", move_id or expect_family, evidence, '
        f"note{agent_note}}} (schema in RUNBOOK.md step 2/3). This asks once "
        f"per session, not every turn.\n"
        f"</daemon>"
    )


def _try_grade_backstop(verdicts_dir, telemetry_path, eval_dir, session_id, agent_id, transcript_path, stale_threshold):
    """One (reason, telemetry_extra) pair if the grade backstop should fire
    for this mailbox right now, else None. Shared by the main-session path
    (agent_id=None, stale_threshold=GRADE_BACKSTOP_STALE_EVENTS) and the
    worker path (DESIGN.md §2h.4: agent_id set, stale_threshold=
    WORKER_ACTIVITY_MIN_EVENTS) — same logic, own sentinel per mailbox, own
    threshold. Never raises (caught by main()'s top-level try/except)."""
    mailbox_key = f"{session_id}.{agent_id}" if agent_id else session_id
    sentinel = os.path.join(verdicts_dir, f"{mailbox_key}.grade-backstop-fired")
    if os.path.exists(sentinel):
        return None
    fires = _session_gradeable_fires(telemetry_path, session_id, agent_id)
    if not fires:
        return None
    graded = _session_grade_count(eval_dir, session_id, agent_id)
    if len(fires) <= graded:
        return None
    events_ago = _events_since(transcript_path, fires[0][2])
    if events_ago <= stale_threshold:
        return None
    try:
        os.makedirs(verdicts_dir, exist_ok=True)
        open(sentinel, "w").close()
    except OSError:
        pass
    return (
        _grade_backstop_reason(len(fires) - graded, events_ago, agent_id),
        {
            "move_id": GRADE_BACKSTOP_MOVE_ID,
            "ungraded_count": len(fires) - graded,
            "oldest_events_ago": events_ago,
        },
    )


def _try_observation_prompt(verdicts_dir, session_id, agent_id, transcript_path, min_events):
    """One (reason, telemetry_extra) pair if the observation-review prompt
    should fire for this mailbox right now, else None. Shared by the main-
    session path (agent_id=None, min_events=OBSERVATION_PROMPT_MIN_EVENTS)
    and the worker path (DESIGN.md §2h.4: agent_id set, min_events=
    WORKER_ACTIVITY_MIN_EVENTS). Never raises."""
    mailbox_key = f"{session_id}.{agent_id}" if agent_id else session_id
    sentinel = os.path.join(verdicts_dir, f"{mailbox_key}.observation-prompt-fired")
    if os.path.exists(sentinel):
        return None
    if _events_since(transcript_path, 0) < min_events:
        return None
    try:
        os.makedirs(verdicts_dir, exist_ok=True)
        open(sentinel, "w").close()
    except OSError:
        pass
    return _observation_prompt_reason(agent_id), {"move_id": OBSERVATION_PROMPT_MOVE_ID}


def _move_muted(move_id, verdicts_dir):
    """Mirror of observer.py's _is_muted for the hook-fired mechanical moves.
    A sleep pass's mute must silence a move at EVERY tier — found 2026-07-07
    (sleep pass 2 night-half) when muting mechanical/unverified-done-claim
    did nothing because this hook never read verdicts/mutes/. Read-only:
    the observer owns expiry cleanup; fail open (missing/unreadable/expired
    = not muted)."""
    path = os.path.join(verdicts_dir, "mutes", move_id.replace("/", "__") + ".json")
    try:
        with open(path, encoding="utf-8") as f:
            mute = json.load(f)
    except (OSError, ValueError):
        return False
    try:
        return float(mute.get("unmute_at", 0)) > time.time()
    except (TypeError, ValueError):
        return False


def main():
    try:
        import valve

        data = json.load(sys.stdin)
        session_id = data.get("session_id")
        if not session_id:
            return

        _sweep_stale_sentinels(valve.VERDICTS_DIR)

        # A turn is one prompt_id regardless of which mailbox (session-level
        # or a worker's) ends up supplying the whisper — the sentinel is
        # keyed on session+prompt, not on the agent-routed mailbox key.
        prompt_id = data.get("prompt_id") or ""
        sentinel_path = os.path.join(valve.VERDICTS_DIR, f"{session_id}.stopblock.{prompt_id}")
        if os.path.exists(sentinel_path):
            return  # this turn already spent its one block

        if data.get("stop_hook_active"):
            return  # possibly-undocumented re-entrancy guard; honor it if present

        agent_id = data.get("agent_id")
        # Same gate as daemon-posttooluse.py: agent-tagged Stop events are
        # fully dark until DESIGN.md §2b's worker-nudges flag is set.
        if agent_id and not valve.worker_nudges_enabled():
            return
        mailbox_key = f"{session_id}.{agent_id}" if agent_id else session_id

        def _block(reason, telemetry_extra):
            try:
                os.makedirs(valve.VERDICTS_DIR, exist_ok=True)
                open(sentinel_path, "w").close()
            except OSError:
                pass
            valve.append_telemetry(
                {
                    "ts": time.time(),
                    "session_id": session_id,
                    "agent_id": agent_id,
                    "event": "injected",
                    "valve": "Stop",
                    **telemetry_extra,
                }
            )
            print(json.dumps({"decision": "block", "reason": reason}))

        # 1. An already-pending, undelivered flag — never wait, never
        # classify synchronously; this only drains what the observer already
        # decided (DESIGN.md §2, RULED).
        block, seq, move_id = valve.pending_injection(mailbox_key, agent_id=agent_id)
        if block:
            valve.write_consumed(mailbox_key, seq)
            _block(block, {"seq": seq, "move_id": move_id})
            return

        # 1b. Catch-up wait (re-ruled 2026-07-05, supersedes the sleep-pass-1
        # classifying-marker wait): at Stop the observer has almost never
        # read the turn-final text yet — it polls every POLL_SECONDS, Stop
        # fires milliseconds after the text lands — so the old marker-only
        # wait missed the common case. Wait for the observer's `.offset`
        # heartbeat to reach the transcript size recorded at Stop entry
        # (double-statted per §2h.6(a) — a single stat raced the harness's
        # write of the final text and read a stale size, see
        # _stable_transcript_size); classification is synchronous inside the
        # observer's drain and the heartbeat is written after the drain
        # returns, so caught-up means every verdict this turn can produce is
        # already on disk. Skips (fails open) when the observer is dead, the
        # heartbeat file doesn't exist, or the transcript is unreadable —
        # gate checked BEFORE the double-stat so skipped sessions never pay
        # the stat gap. Main session only — workers' transcripts aren't
        # heartbeat-tracked.
        transcript_path = data.get("transcript_path")
        if not agent_id and transcript_path:
            offset_path = os.path.join(valve.VERDICTS_DIR, f"{session_id}.offset")
            target = None
            if os.path.exists(offset_path) and valve.observer_alive(session_id):
                target = _stable_transcript_size(transcript_path)
            if target is not None:
                # CONVERT the wait: poke the observer to classify the turn-final
                # window now (and first), instead of waiting for it to get there
                # on its own poll cadence. See _write_poke's block comment above.
                poke_path = os.path.join(valve.VERDICTS_DIR, f"{session_id}.classify-now")
                _write_poke(poke_path, target)
                deadline = time.time() + STOP_WAIT_CAP_S
                while True:
                    # Verdicts land inside the drain, before the heartbeat
                    # moves — check the mailbox first so a whisper delivers
                    # the moment it exists, not a poll later.
                    block, seq, move_id = valve.pending_injection(mailbox_key, agent_id=agent_id)
                    if block:
                        _clear_poke(poke_path)
                        valve.write_consumed(mailbox_key, seq)
                        _block(block, {"seq": seq, "move_id": move_id, "stop_wait": True})
                        return
                    # The observer removes the poke once it has classified
                    # through `target`. Poke gone with no verdict above ⇒ the
                    # turn-final text was judged and carried no drift; stop
                    # waiting rather than burning the cap (converts the no-fire
                    # majority — the §2h.6 latency tax).
                    if not os.path.exists(poke_path):
                        break
                    try:
                        with open(offset_path, encoding="utf-8") as f:
                            drained = int(f.read().strip() or "0")
                    except (OSError, ValueError):
                        break
                    if drained >= target or time.time() >= deadline:
                        break
                    time.sleep(0.25)
                # Caught up (or capped): one final mailbox read for a verdict
                # written by the drain that closed the gap.
                _clear_poke(poke_path)
                block, seq, move_id = valve.pending_injection(mailbox_key, agent_id=agent_id)
                if block:
                    valve.write_consumed(mailbox_key, seq)
                    _block(block, {"seq": seq, "move_id": move_id, "stop_wait": True})
                    return

        # 2. No pending flag: deterministic, valve-selected mechanical check.
        if not transcript_path or not os.path.exists(transcript_path):
            return
        if _announced_not_started(transcript_path):
            reason = valve.build_block({"move_id": "mechanical/announced-not-started"}, agent_id=agent_id)
            if not reason:
                return
            _block(reason, {"move_id": "mechanical/announced-not-started"})
            return

        # 2b. mechanical/ungrounded-chat-claim (DESIGN.md §2h.1): a zero-
        # tool-call turn asserting repo facts this session never opened.
        # Deterministic, never the classifier — chat turns are exactly where
        # the classifier's Stop catch-up wait has nothing to catch (no tool
        # events means no window ever closes to judge). Priority behind the
        # pending-whisper and announced-not-started checks above (one
        # whisper per Stop). Applies to main session and, behind the
        # worker-nudges flag already gated above, worker Stop events alike —
        # same reach as announced-not-started just above.
        ungrounded = _ungrounded_chat_claim(transcript_path)
        if ungrounded:
            reason = valve.build_block({"move_id": "mechanical/ungrounded-chat-claim"}, agent_id=agent_id)
            if reason:
                _block(reason, {"move_id": "mechanical/ungrounded-chat-claim", "artifact": ungrounded})
                return

        # 2c. mechanical/unverified-done-claim (DESIGN.md §2h.6(c)): the turn
        # ends on a completion claim, mutated something non-git, and ran
        # nothing where the claim could fail. Zero-latency sibling of
        # anchor/verify-claim for the literal done-claim form — 8 of the 11
        # capped-wait late fires in the forensics were this family, and a
        # done-claim is structurally turn-final, so it always races the Stop
        # wait. Priority per §2h.6(c): pending whisper > announced-not-
        # started > ungrounded-chat-claim > this (the two mechanical checks
        # above are mutually exclusive with this one in practice — zero vs.
        # nonzero tool calls — but the order is the contract regardless).
        if not _move_muted("mechanical/unverified-done-claim", valve.VERDICTS_DIR) and _unverified_done_claim(transcript_path):
            reason = valve.build_block({"move_id": "mechanical/unverified-done-claim"}, agent_id=agent_id)
            if reason:
                _block(reason, {"move_id": "mechanical/unverified-done-claim"})
                return

        # 3/4. Grade-backstop + observation-review prompt. DESIGN.md §2h.4
        # extends both to worker Stop events (agent_id set, already gated on
        # the worker-nudges flag above) with their own per-(session, agent_id)
        # sentinels and a lower activity threshold — workers run far shorter
        # than the main session, so the main-session gates (both 40 events)
        # would exempt nearly every real worker. Main session keeps its
        # original constants and sentinel names (agent_id=None collapses the
        # mailbox key back to session_id) — byte-identical behavior to before
        # this section existed.
        stale_threshold = WORKER_ACTIVITY_MIN_EVENTS if agent_id else GRADE_BACKSTOP_STALE_EVENTS
        min_events = WORKER_ACTIVITY_MIN_EVENTS if agent_id else OBSERVATION_PROMPT_MIN_EVENTS

        result = _try_grade_backstop(
            valve.VERDICTS_DIR, valve.TELEMETRY_PATH, valve.EVAL_DIR, session_id, agent_id, transcript_path, stale_threshold
        )
        if result:
            _block(*result)
            return

        result = _try_observation_prompt(valve.VERDICTS_DIR, session_id, agent_id, transcript_path, min_events)
        if result:
            _block(*result)
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
