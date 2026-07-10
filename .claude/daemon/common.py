"""Shared windowing + classifier-call code for the daemon observer.

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
import shlex
import subprocess
import tempfile
import time
from datetime import datetime
from pathlib import Path

CADENCE_EVENTS = 8
CADENCE_SECONDS = 90
MODEL = "claude-haiku-4-5-20251001"

# DESIGN.md §4c-1: bumped whenever WindowState's ledger/verdict shape changes,
# so §4b/sleep-pass scoring never silently mixes pre- and post-change regimes.
# v3 (§2c tier 3): Edit/Write/MultiEdit ledger lines gain an "(adds: ...)"
# annotation from STOPGAP_MARKERS.
# v4: TASK/RECENT hard-capped + harness-injected texts excluded from TASK.
# v5 (§2f): SESSION FACTS block (last verification per class, latest
# TASK-set context switch, edited-path last-read) appended to window text.
# v6 (§2f): recent plain-Read facts ("read <path>: event N, M events ago")
# added to SESSION FACTS — grounding reads scroll out of the ~8-event ledger
# and ungrounded-resolution was FP-firing on claims that were in fact
# grounded by a Read (live FP 2026-07-05, session a5b78b70 seq 2).
# v7 (TICKETS.md T10): ledger lines carry a (hook-warning: ...) annotation
# when a tool result carries a shared-checkout-guard or landing-protocol
# hook warning — wakes anchor/unheeded-warning.
WINDOW_VERSION = 7

# Window-size discipline (2026-07-04 orchestrator incident, session cadd7aad):
# a <task-notification> embedding a worker's full report became current_task
# verbatim, and untruncated assistant texts rode along in RECENT — window text
# grew to hundreds of KB and every classifier call after 14:00 timed out (nine
# `classifier error: timeout` entries; the daemon was blind for two hours).
# TASK/RECENT are context for a judgment call, not an archive: hard-cap them.
TASK_MAX_CHARS = 800
RECENT_MAX_CHARS = 1500

# §2f v6: how many most-recent Read paths render in SESSION FACTS. Small on
# purpose — the clause exists to keep a *recent* grounding read visible just
# past the ledger horizon, not to archive every file the session opened.
READ_FACTS_MAX = 5

# Harness-injected user texts — subagent completion notifications, hook
# reminders, slash-command echoes — are not instructions from the human and
# must never become the TASK line. Checked against the stripped text's start.
HARNESS_TEXT_PREFIXES = (
    "<task-notification>",
    "<system-reminder>",
    "<local-command",  # <local-command-caveat>, <local-command-stdout>
    "<command-name>",
)

# Tool names whose repeated targets get a "(Nth touch this session)" ledger
# annotation (§4c-1). Deliberately narrow to what the spec names — Read/Edit
# thrash is the documented tell; widening to every tool is a judgment call
# for a later sleep pass, not a guess to make now.
REPEAT_TARGET_TOOLS = ("Read", "Edit")

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
    """Parse moves.md into {move_id: {"signature", "cooldown", "kind", "payload"}}.
    `kind` is "alert" unless the entry carries `- **kind:** advice` (the
    priming tier, §2e) — advice moves get the <daemon-advice> wrapper, no
    supervised-mode ack, and never escalate."""
    moves = {}
    for block in re.split(r"(?m)^## ", text)[1:]:
        lines = block.splitlines()
        move_id = lines[0].strip()
        body = "\n".join(lines[1:])
        sig_m = re.search(r"-\s*\*\*signature:\*\*\s*(.*?)(?=\n-\s*\*\*cooldown:\*\*)", body, re.DOTALL)
        cd_m = re.search(r"-\s*\*\*cooldown:\*\*\s*(\S+)", body)
        kind_m = re.search(r"-\s*\*\*kind:\*\*\s*(\S+)", body)
        pl_m = re.search(r"-\s*\*\*payload:\*\*\s*\n(.*)", body, re.DOTALL)
        signature = re.sub(r"\s+", " ", sig_m.group(1)).strip() if sig_m else ""
        cooldown = cd_m.group(1).strip() if cd_m else "standard"
        kind = kind_m.group(1).strip() if kind_m else "alert"
        payload = pl_m.group(1).strip() if pl_m else ""
        # payload lines are blockquoted with "> " — strip that for the injected text
        payload = "\n".join(l[2:] if l.startswith("> ") else l for l in payload.splitlines()).strip()
        moves[move_id] = {"signature": signature, "cooldown": cooldown, "kind": kind, "payload": payload}
    return moves


def validate_move_id(move_id, moves):
    """Returns move_id if it's a real, classifier-selectable move in the
    catalog, else None. `escalate/*` and `mechanical/*` ids are daemon/valve-
    selected only and are never valid coming from the classifier; `phase/*`
    (DESIGN.md §2d, phase-transition tier) joins that list — those fire from
    deterministic rules over the phase stream, never from Haiku. An
    unrecognized id — e.g. the hallucinated `coaching/scope-drift` (nonexistent;
    only `anchor/scope-drift` is real) seen in replay round 2 — must be treated
    as clear, not silently accepted under a default cooldown class."""
    if not move_id or move_id not in moves or move_id.startswith(("escalate/", "mechanical/", "phase/")):
        return None
    return move_id


def build_signature_catalog(moves):
    lines = []
    for mid in sorted(moves):
        if mid.startswith(("escalate/", "mechanical/", "phase/")):
            continue  # daemon/valve-selected, never offered to the classifier
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


# DESIGN.md §2c: agents narrate their hacks, which makes the highest-precision
# detector a regex over what an edit ADDS, not a classifier call. Categories
# match moves.md's mechanical/confessed-stopgap signature verbatim.
STOPGAP_MARKERS = {
    "hack-word": re.compile(r"\b(?:HACK|XXX|kludge)\b", re.IGNORECASE),
    "workaround": re.compile(r"\bworkaround\b", re.IGNORECASE),
    "for-now": re.compile(
        r"\bfor now\b|\btemporar(?:y|ily)\b|\bquick fix\b|\bstopgap\b|\bband-?aid\b",
        re.IGNORECASE,
    ),
    "deferral": re.compile(
        r"\bFIXME\b|\bTODO\b.{0,40}?\b(?:proper|real fix|later|revisit)\b",
        re.IGNORECASE | re.DOTALL,
    ),
    "lint-suppression": re.compile(r"#\[allow\("),
    "race-sleep": re.compile(r"\bthread::sleep\s*\(|\bsleep\s*\("),
}

# TICKETS.md T2(b), resolved by the orchestrator via direct forensic
# investigation (not re-graded here): the a5d63eee seq-6 fire's grading note
# ("the flagged 'yet' referenced a pre-existing coverage gap... no confessed
# shortcut in the diff") misattributes the artifact. The session's surviving
# observer log (verdicts/a5d63eee-1b49-424e-aa9b-775a3e64b7d1.log, still on
# disk) shows the real evidence line: "mechanical/confessed-stopgap fired:
# Edit .../crates/manifold-renderer/src/generators/mesh_pipeline.rs
# hits=['lint-suppression']" — a genuine, unexempted #[allow(unreachable_code)]
# behind a new early `return;`. "yet"/"harness"/"headless" appear in ZERO
# Edit/Write/MultiEdit diffs anywhere in that session's transcript (confirmed
# via jq), so the grading note describes a different specimen than what this
# detector actually matched. Whether the mesh_pipeline.rs edit itself is a
# true or false positive was never actually graded — that's a separate,
# still-open grading gap, not something this ticket's self-disposal
# exemption changes (no disposal trigger appears near that edit either way).

# Markdown and the daemon's own internals are excluded from the scan entirely
# (DESIGN.md §2c) — the daemon narrating its own build in prose isn't a hack.
STOPGAP_EXCLUDED_PATH_RE = re.compile(r"(^|/)\.claude/|\.md$", re.IGNORECASE)

# race-sleep only fires outside test code (§2c) — a real thread::sleep/sleep()
# call in a test file is normal test scaffolding, not a race workaround.
STOPGAP_TEST_PATH_RE = re.compile(
    r"(^|/)tests?/|(^|/)test_[^/]+\.\w+$|_test\.\w+$", re.IGNORECASE
)

# TICKETS.md T2: an added marker whose surrounding added text ALSO names its
# own concrete disposal trigger is eval-loop scaffolding, not a confession —
# e.g. "TEMPORARY: delete after the fixtures-freeze lands" names exactly when
# it goes away, unlike a bare "for now" with no disposal condition attached.
# Per-pair, not global: only the pair whose OWN added text carries a disposal
# trigger is exempted.
DISPOSAL_TRIGGER_RE = re.compile(
    r"\bdelete\s+(?:after|once|when)\b|\bconvert\s+to\b|\buntil\s+\S|\bretire[sd]?\s+(?:with|after)\b",
    re.IGNORECASE,
)

# DESIGN.md §2h.2: mechanical/landing-doc-reflex's docs-only suppression.
# Distinct from STOPGAP_EXCLUDED_PATH_RE above (which excludes ALL markdown +
# .claude/ from stopgap scanning) — this one names exactly the three path
# families moves.md's signature calls the paper trail: docs/, memory (the
# gitignored auto-memory files under ~/.claude/projects/.../memory/), and
# .claude/ internals.
LANDING_DOCS_ONLY_RE = re.compile(r"(^|/)(?:docs|memory|\.claude)/", re.IGNORECASE)


def is_docs_memory_or_claude_path(path):
    return bool(path) and bool(LANDING_DOCS_ONLY_RE.search(path))


# TICKETS.md T9 / mechanical/stale-brief: how old a queue/brief/agenda/handoff
# artifact's mtime must be, at read time, before the move fires.
STALE_BRIEF_MAX_AGE_S = 48 * 60 * 60


def is_stale_brief_path(path):
    """TICKETS.md T9 / mechanical/stale-brief: a queue, brief, agenda, or
    handoff artifact whose staleness this move warns about. Basename-scoped
    except for the docs/handoff* and moves.md-adjacent memory-file cases."""
    if not path:
        return False
    base = os.path.basename(path)
    if base.endswith("_QUEUE.md"):
        return True
    if "BRIEF" in base.upper() and base.endswith(".md"):
        return True
    if re.match(r"^PASS.*_AGENDA\.md$", base, re.IGNORECASE):
        return True
    if "docs/handoff" in path.lower():
        return True
    if re.match(r"^handoff_.*\.md$", base, re.IGNORECASE):
        return True
    return False


def _stopgap_hits(text):
    return {cat for cat, pat in STOPGAP_MARKERS.items() if pat.search(text or "")}


def detect_stopgap_markers(name, input_):
    """Confession-marker categories an Edit/Write/MultiEdit ADDS to a file,
    absent from whatever it replaces — DESIGN.md §2c tiers 1/3. Removing a
    hack must never fire: only `new_string`/`content` counts as "added";
    `old_string` is the baseline subtracted out, per edit pair for MultiEdit.
    Returns a sorted list of category names (possibly empty); never raises."""
    if name not in ("Edit", "Write", "MultiEdit") or not isinstance(input_, dict):
        return []
    path = input_.get("file_path") or ""
    if STOPGAP_EXCLUDED_PATH_RE.search(path):
        return []

    if name == "Write":
        pairs = [(input_.get("content") or "", "")]
    elif name == "Edit":
        pairs = [(input_.get("new_string") or "", input_.get("old_string") or "")]
    else:  # MultiEdit
        edits = input_.get("edits")
        if not isinstance(edits, list):
            return []
        pairs = [
            (e.get("new_string") or "", e.get("old_string") or "")
            for e in edits
            if isinstance(e, dict)
        ]

    hits = set()
    for added, removed in pairs:
        pair_hits = _stopgap_hits(added) - _stopgap_hits(removed)
        if pair_hits and DISPOSAL_TRIGGER_RE.search(added):
            continue  # self-disposing marker — exempt (TICKETS.md T2, 2026-07-07 contract)
        hits |= pair_hits

    if "race-sleep" in hits and STOPGAP_TEST_PATH_RE.search(path):
        hits.discard("race-sleep")
    return sorted(hits)


# DESIGN.md §2f: the session-fact store's verification-class detector — same
# regex tier as STOPGAP_MARKERS above, no semantic check on what a command
# actually covered. render-read is keyed on Read's target suffix, not a
# command regex, so it's handled separately in detect_verification_class.
VERIFICATION_MARKERS = {
    "test-run": re.compile(
        r"\bcargo\s+(?:test|bench)\b|\bpytest\b|\bnpm\s+test\b|\bgo\s+test\b|\bswift\s+test\b",
        re.IGNORECASE,
    ),
    "lint": re.compile(r"\bcargo\s+clippy\b|\beslint\b|\bruff\b|\bflake8\b|\bpylint\b|\bmypy\b", re.IGNORECASE),
    "script-run": re.compile(
        r"\bcargo\s+run\b|\bnpm\s+run\b|\bpython3?\s+\S+\.py\b|\./\S+\.sh\b|\bnode\s+\S+\.js\b",
        re.IGNORECASE,
    ),
}


def detect_verification_class(name, input_):
    """Returns "test-run" / "lint" / "script-run" / "render-read", or None.
    Bash commands match test-run > lint > script-run (first hit wins — a
    lint invocation is never also counted as a test-run); a Read of a *.png
    is render-read regardless of the Bash categories. Never raises."""
    if not isinstance(input_, dict):
        return None
    if name == "Read":
        path = input_.get("file_path") or ""
        return "render-read" if path.lower().endswith(".png") else None
    if name != "Bash":
        return None
    cmd = input_.get("command") or ""
    for cls in ("test-run", "lint", "script-run"):
        if VERIFICATION_MARKERS[cls].search(cmd):
            return cls
    return None


# .claude/GIT_TREE_DISCIPLINE.md §2 (2026-07-04): the ff-only "main = pointer"
# model produced twin commits under concurrent orchestrators — the same
# content merged onto main once and re-committed onto a live branch again
# under different SHAs. The landing protocol's two twin-killers name the
# operations that create or hide a twin: cherry-picking content that already
# exists as commits on a live branch, and deleting a branch before its
# content is confirmed on main. preToolUseBash.py's §1b guard deterministically
# gates the exact push/merge-to-main commands; this table covers the two
# operations that guard doesn't (cherry-pick isn't main-checkout-scoped by
# nature, and branch deletion has no "target" to detect at all) — added at
# Peter's suggestion the same session the incident was diagnosed.
def detect_git_landing_signal(name, input_):
    """Bash commands running the two twin-commit-prone git operations
    (mechanical/git-landing signature in moves.md), anchored to command
    position via the shared segment splitter (TICKETS.md T1 — the prior
    regex scanned the raw string anywhere, so `rg 'git branch -D'` — a
    SEARCH for that text, no git invocation at all — falsely fired).
    A branch-delete gated by a `git merge-base --is-ancestor` segment in the
    same compound command is exempt (the protocol-compliant delete — see the
    ancestor_guard note below). Never raises. Returns a sorted list of category
    names (possibly empty)."""
    if name != "Bash" or not isinstance(input_, dict):
        return []
    cmd = input_.get("command") or ""
    hits = set()
    ancestor_guard = False
    try:
        for toks in _landing_shlex_segments(cmd):
            toks = _landing_strip_leading_keywords(toks)
            if not toks or toks[0] != "git":
                continue
            _target_dir, sub, rest = _landing_git_checkout_dir(toks, ".")
            if sub == "cherry-pick":
                hits.add("cherry-pick")
            elif sub == "branch" and any(t in ("-d", "-D", "--delete") for t in rest):
                hits.add("branch-delete")
            elif sub == "push" and any(t == "--delete" or t.startswith("--delete=") for t in rest):
                hits.add("branch-delete")
            elif sub == "merge-base" and "--is-ancestor" in rest:
                ancestor_guard = True
    except Exception:
        return []
    # eval/observations.session.jsonl (2026-07-10) + 8 graded FPs across the
    # graded week: a branch-delete &&-gated on `git merge-base --is-ancestor`
    # in the SAME compound command is exactly the protocol
    # GIT_TREE_DISCIPLINE.md §2 requires ("never delete a branch until
    # merge-base --is-ancestor confirms its commits are on main"). Firing on a
    # compliant, guarded delete trains alert-blindness on git-landing's
    # highest-frequency form (26 fires/week). Deterministic, regex-tier: a
    # merge-base --is-ancestor segment anywhere in the same command string
    # exempts the branch-delete. cherry-pick — the other twin-killer, unrelated
    # to ancestry — is never exempted.
    if ancestor_guard:
        hits.discard("branch-delete")
    return sorted(hits)


# DESIGN.md §2h.2: mechanical/landing-doc-reflex. The token-parsing below
# (quote-aware segment splitting, control-flow/env-assignment stripping, the
# `-C <dir>` chain resolver, the explicit-refspec string match) is PORTED
# from preToolUseBash.py's landing-protocol guard (_shlex_segments,
# _strip_leading_keywords, _git_checkout_dir, _push_targets_main) rather
# than re-derived — read that file, never edit it. One deliberate
# divergence: that guard resolves an implicit (no-refspec) push/merge's
# current branch via a live `git rev-parse` subprocess (_current_branch);
# this observer runs no git subprocesses at all (DESIGN.md §2h.2), so the
# no-refspec case is instead resolved from the transcript's own per-entry
# `gitBranch`/`cwd` fields — confirmed against real transcripts to update
# live across branch switches within a session, so it's exactly as fresh as
# a subprocess call would be at that point in the replay, for free.
_LANDING_SHELL_OPERATORS = {"&&", "||", ";", "|", "&"}
_LANDING_DATA_KEYWORDS = {"for", "select", "case", "in", "function"}
_LANDING_STRIP_KEYWORDS = {
    "if", "then", "elif", "else", "fi", "while", "until", "do", "done",
    "esac", "time", "!", "{", "}", "(", ")",
}
_MAIN_REF_TOKENS = ("main", "refs/heads/main")


def _landing_shlex_segments(cmd):
    """Ported verbatim from preToolUseBash.py's _shlex_segments: quote-aware
    tokenize + split into command-position segments on shell operators."""
    try:
        tokens = shlex.split(cmd, posix=True)
    except ValueError:
        return []
    segments = []
    current = []
    for t in tokens:
        if t in _LANDING_SHELL_OPERATORS:
            if current:
                segments.append(current)
            current = []
        else:
            current.append(t)
    if current:
        segments.append(current)
    return segments


def _landing_strip_leading_keywords(toks):
    """Ported verbatim from preToolUseBash.py's _strip_leading_keywords."""
    while toks:
        t = toks[0]
        if t in _LANDING_DATA_KEYWORDS:
            return []  # data list, not a command
        if t in _LANDING_STRIP_KEYWORDS:
            toks = toks[1:]
            continue
        if re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", t):
            toks = toks[1:]
            continue
        break
    return toks


def _landing_git_checkout_dir(toks, cwd):
    """Ported verbatim from preToolUseBash.py's _git_checkout_dir: resolves a
    chain of `-C <dir>` flags against a baseline cwd (git semantics: each is
    relative to the last). Returns (target_dir, sub, rest_toks), or
    (None, None, None) if unparsable."""
    i = 1
    target = Path(cwd)
    while i < len(toks) and toks[i].startswith("-"):
        if toks[i] == "-C":
            if i + 1 >= len(toks):
                return None, None, None
            p = Path(toks[i + 1])
            target = p if p.is_absolute() else (target / p)
            i += 2
        elif toks[i] == "-c":
            i += 2
        else:
            i += 1
    sub = toks[i] if i < len(toks) else ""
    return target, sub, toks[i + 1 :]


def _landing_push_targets_main_explicit(rest_toks):
    """Explicit-refspec branch of preToolUseBash.py's _push_targets_main,
    ported verbatim (pure string match: 2+ positional args means an explicit
    refspec was given). Returns True/False for an explicit refspec, or None
    when there's 0/1 positional args (bare push / push-with-remote-only) —
    the caller resolves that ambiguous case from the transcript's own
    gitBranch instead of a subprocess."""
    positional = [t for t in rest_toks if not t.startswith("-")]
    if len(positional) >= 2:
        refspec = positional[-1]
        remote_part = refspec.split(":", 1)[-1] if ":" in refspec else refspec
        return remote_part in _MAIN_REF_TOKENS
    return None


def _landing_same_directory(target_dir, cwd):
    """True if a parsed `-C` target resolves back to the session's own cwd
    (or no `-C` was present at all, in which case target_dir IS Path(cwd)).
    Any resolve failure (e.g. a worktree directory no longer on disk) fails
    open to False — abstain, don't guess."""
    try:
        return target_dir.resolve() == Path(cwd).resolve()
    except (OSError, ValueError):
        return target_dir == Path(cwd)


def detect_landing_on_main(name, input_, cwd, git_branch):
    """DESIGN.md §2h.2 / moves.md mechanical/landing-doc-reflex: a live Bash
    command that lands work on main — `git merge` run while on main, or
    `git push` whose refspec (or, absent one, current branch) is main.
    `cwd`/`git_branch` are the transcript entry's own fields for the tool
    event being checked (this observer runs no git subprocesses). Only the
    no-`-C` case (the git command running directly in the session's own
    working directory) is resolved against `git_branch` — a `-C <other-dir>`
    command abstains unless that dir resolves back to `cwd`, since there is
    no subprocess here to check some OTHER directory's branch (a documented
    residue: an orchestrator landing on main via `git -C <path>` from a
    session whose own cwd is elsewhere is invisible to this check). Returns
    a sorted list of category names ("merge", "push"; possibly both,
    possibly empty); never raises."""
    if name != "Bash" or not isinstance(input_, dict) or not cwd:
        return []
    cmd = input_.get("command") or ""
    hits = set()
    for toks in _landing_shlex_segments(cmd):
        toks = _landing_strip_leading_keywords(toks)
        if not toks or toks[0] != "git":
            continue
        target_dir, sub, rest = _landing_git_checkout_dir(toks, cwd)
        if target_dir is None:
            continue
        on_main = git_branch == "main" and _landing_same_directory(target_dir, cwd)
        if sub == "merge":
            if on_main:
                hits.add("merge")
        elif sub == "push":
            explicit = _landing_push_targets_main_explicit(rest)
            if explicit is True or (explicit is None and on_main):
                hits.add("push")
    return sorted(hits)


# DESIGN.md §2c-ask: a distinct vocabulary from STOPGAP_MARKERS above — that
# set screens code diffs for confessed hacks; this one screens an
# AskUserQuestion's own option text for the shortcut-recommended-over-
# root-fix framing CLAUDE.md's fix-at-the-root rule forbids (2026-07-04
# incident: "approximate, no new primitive (Recommended)" vs "real transform
# primitive"). Kept in this file, not the hook, so moves.md/common.py stay
# the single vocabulary source.
SHORTCUT_WORDS_RE = re.compile(
    r"\b(?:approximat\w*|minimal|stopgap|for now|quick(?:\s+fix)?|defer(?:red)?|"
    r"later|without\s+(?:a|the)\s+new\b|no\s+new\s+\w+)\b",
    re.IGNORECASE,
)
ROOT_FIX_WORDS_RE = re.compile(
    r"\b(?:proper|full|real|fundamental|redesign|new\s+primitive|root\s*cause|"
    r"correct(?:ly)?\s+fix|complete\s+fix)\b",
    re.IGNORECASE,
)
RECOMMENDED_MARKER_RE = re.compile(r"\(recommended\)", re.IGNORECASE)


def detect_shortcut_fork(tool_input):
    """AskUserQuestion PreToolUse screen (DESIGN.md §2c-ask). Fires when one
    question offers an option labeled "(Recommended)" whose label+description
    carries shortcut vocabulary, while a different option in the same
    question carries root-fix vocabulary — the exact shortcut-as-
    recommendation framing the 2026-07-04 incident exhibited. Returns the
    list of matching question texts (possibly empty); never raises."""
    if not isinstance(tool_input, dict):
        return []
    questions = tool_input.get("questions")
    if not isinstance(questions, list):
        return []

    hits = []
    for q in questions:
        if not isinstance(q, dict):
            continue
        options = q.get("options")
        if not isinstance(options, list) or len(options) < 2:
            continue

        recommended = None
        for opt in options:
            if isinstance(opt, dict) and RECOMMENDED_MARKER_RE.search(opt.get("label") or ""):
                recommended = opt
                break
        if recommended is None:
            continue

        rec_text = f"{recommended.get('label') or ''} {recommended.get('description') or ''}"
        if not SHORTCUT_WORDS_RE.search(rec_text):
            continue

        others_text = " ".join(
            f"{o.get('label') or ''} {o.get('description') or ''}"
            for o in options
            if isinstance(o, dict) and o is not recommended
        )
        if ROOT_FIX_WORDS_RE.search(others_text):
            hits.append(q.get("question") or "")

    return hits


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


def _clip(s, n):
    """Head-truncate long free text for window assembly. Unlike `_truncate`
    (one-line ledger targets), keeps newlines — TASK/RECENT prose structure
    is signal for the classifier."""
    return s if len(s) <= n else s[: n - 1] + "…"


def ordinal(n):
    """1 -> '1st', 2 -> '2nd', 3 -> '3rd', 4 -> '4th', 11-13 -> '11th'/'12th'/'13th'."""
    if 10 <= n % 100 <= 20:
        suffix = "th"
    else:
        suffix = {1: "st", 2: "nd", 3: "rd"}.get(n % 10, "th")
    return f"{n}{suffix}"


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


# TICKETS.md T10 / anchor/unheeded-warning: a PreToolUse hook's
# additionalContext (shared-checkout guard warning, landing-protocol
# reminder — preToolUseBash.py) is appended as extra text inside the SAME
# tool_result's own content the hook fired on. These are the two literal
# opening fragments preToolUseBash.py's shared_checkout_guard and
# LANDING_PROTOCOL_REMINDER emit — kept here, not re-derived, so a wording
# change in the hook is a one-place update.
HOOK_WARNING_MARKERS = (
    "Heads-up: branch-switch in the shared main checkout",
    "Landing on main. Protocol",
)


def extract_hook_warning(tool_result_content_text):
    """Returns the first ~80 chars of the matched hook-warning marker text if
    present in a tool_result's own content, else None. Never raises."""
    text = tool_result_content_text or ""
    for marker in HOOK_WARNING_MARKERS:
        idx = text.find(marker)
        if idx != -1:
            return _truncate(text[idx:], 80)
    return None


TASK_ADDRESSED_MIN_CHARS = 40  # a trivial ack ("OK.", "Got it.") doesn't count as addressing TASK


def format_window(task, ledger, recent, task_addressed=False, events_since_task=0, session_facts=""):
    task_str = task if task else "(no task statement yet)"
    if task:
        task_str += f" ({events_since_task} tool events since set)"
        if task_addressed:
            task_str += " — already addressed by a prior reply this session"
    ledger_str = " · ".join(ledger) if ledger else "(no tool events)"
    recent_str = "\n---\n".join(t.strip() for t in recent) if recent else "(no assistant text yet)"
    base = f'TASK: "{task_str}"\n\nLEDGER: `{ledger_str}`\n\nRECENT:\n{recent_str}'
    if session_facts:
        base += f"\n\nSESSION FACTS: {session_facts}"
    return base


def format_session_facts(state, current_event_count):
    """DESIGN.md §2f: renders the durable, regex-extracted facts store as one
    clause list appended to the window text. The ledger only covers the last
    ~8 events; a verifying/context-setting event from many windows back
    (sleep-pass-1's dominant FP class — "the verifying event existed but sat
    beyond the window ledger") stays visible here for the rest of the
    session. Bounded regardless of session length: at most one clause per
    verification class, at most READ_FACTS_MAX recent reads, only the single
    latest context switch, and only paths edited in the window that's
    closing right now."""
    parts = []
    for cls in ("test-run", "lint", "script-run", "render-read"):
        fact = state.last_verification.get(cls)
        if not fact:
            continue
        ec, label = fact
        parts.append(f"last {cls}: event {ec}, {current_event_count - ec} events ago ({label})")
    # Recent plain reads (v6): a claim grounded by a Read that scrolled past
    # the ledger horizon needs its provenance visible, or ungrounded-resolution
    # FP-fires on it. Path stays in the clause so a stale unrelated read never
    # becomes a blanket alibi — the classifier must match claim subject to path.
    for path, ec in sorted(state.last_read_event.items(), key=lambda kv: -kv[1])[:READ_FACTS_MAX]:
        parts.append(f"read {path}: event {ec}, {current_event_count - ec} events ago")
    if state.context_switches:
        ec, label = state.context_switches[-1]
        parts.append(f"TASK set by user at event {ec}" + (f' ("{label}")' if label else ""))
    for path, read_ec in state.edits_with_prior_read_this_window:
        parts.append(f"file {path} last read event {read_ec}")
    return "; ".join(parts)


# DESIGN.md §2 / PASS2_AGENDA item 3 (sleep pass 2): anchor/verify-claim's
# biggest and cleanest FP class (night-half taxonomy class (a), ~11/50 graded
# fires) is windows with NO stated claim in view — recon reads, task-start
# UserPromptSubmit windows, waiting-on-TaskOutput — where the classifier fired
# on tool-heavy cadence rather than an actual assertion (#32 cadence-fire,
# CONFIRMED). This is a deterministic claim-PRESENCE gate over the window's
# RECENT assistant text: verify-claim may only fire when the assistant actually
# stated a completion / success / verification claim (the kind of statement
# verify-claim exists to check). Suppression-ONLY — the observer applies it as
# a post-filter that can only REMOVE a verify-claim fire, never add one, so it
# cannot re-introduce any prior noise class; its only risk is over-suppression
# (recall), which the vocabulary is kept deliberately BROAD to bound (a
# too-broad match errs toward KEEPING the classifier's fire, never toward
# suppressing a real TP). Recon narration ("let me read X", "checking Y",
# questions, "waiting for the agent") carries none of these markers.
CLAIM_MARKERS_RE = re.compile(
    r"\b(?:"
    r"done|fix(?:ed|es)?|works?|working|land(?:ed|s)?|shipp(?:ed|ing)|pushed|"
    r"implement(?:ed|s)?|completed?|complete|resolv(?:ed|es)?|ready|"
    r"passes|passing|passed|verif(?:ied|ies)|confirm(?:ed|s)?|correct(?:ly)?|"
    r"succeed(?:s|ed)?|solved?|functional|"
    r"wired\s+up|in\s+place|good\s+to\s+go|"
    r"should\s+(?:now\s+)?(?:work|be|fix|resolve|handle|pass)|"
    r"now\s+(?:works?|renders?|handles?|passes|correct)|"
    r"is\s+(?:now\s+)?(?:correct|working|fixed|resolved|done|right)"
    r")\b",
    re.IGNORECASE,
)


def contains_claim(texts):
    """Deterministic claim-presence gate for anchor/verify-claim (PASS2 item 3,
    class (a) FP fix). `texts` is the window's RECENT assistant texts. Returns
    True if any reads as a completion / success / verification assertion. Kept
    broad on purpose: erring toward True keeps the classifier's judgment (safe),
    erring toward False would suppress a real fire (recall loss). Never raises."""
    for t in texts or ():
        if isinstance(t, str) and CLAIM_MARKERS_RE.search(t):
            return True
    return False


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

        # DESIGN.md §4c-1: arithmetic the classifier can't do reliably,
        # computed here and annotated into the ledger/TASK line instead.
        self.target_touch_counts = {}  # "Read:path" -> count, REPEAT_TARGET_TOOLS only
        self.consecutive_failures = 0  # current err streak, across all tools
        self.events_since_task = 0  # tool events since current_task was last set

        # DESIGN.md §2f: session-fact store. Durable for the whole session
        # (never trimmed to a window horizon, unlike ledger/recent_texts) —
        # deterministic functions of the transcript, so a catchup replay
        # rebuilds them identically; no firestate persistence needed on top
        # of that (verified: catchup always replays from offset 0).
        self.last_verification = {}  # class -> (event_count, label)
        self.context_switches = []  # [(event_count, label)] — every TASK-set event
        self.last_read_event = {}  # path -> event_count
        self.last_edit_event = {}  # path -> event_count
        self.edits_with_prior_read_this_window = []  # [(path, read_event_count)], reset per window

    def _annotate_ledger(self, name, target, status, stopgap_hits=None, hook_warning=None):
        """Returns a "(...)" suffix or "" — the repeat-touch and failure-streak
        tells from §4c-1, plus (§2c tier 3) confession-marker categories an
        Edit/Write/MultiEdit just added, plus (T10) a hook warning attached to
        this tool result. Only fires on a repeat (n>=2) / a streak (n>=2): a
        first touch or a lone failure isn't a pattern worth flagging; a marker
        hit or hook warning always annotates (each is already a discrete
        event, not a running count)."""
        notes = []
        if name in REPEAT_TARGET_TOOLS and target:
            key = f"{name}:{target}"
            self.target_touch_counts[key] = self.target_touch_counts.get(key, 0) + 1
            n = self.target_touch_counts[key]
            if n > 1:
                notes.append(f"({ordinal(n)} touch this session)")
        if status == "err":
            self.consecutive_failures += 1
            if self.consecutive_failures > 1:
                notes.append(f"({ordinal(self.consecutive_failures)} consecutive failure)")
        else:
            self.consecutive_failures = 0
        if stopgap_hits:
            notes.append(f"(adds: {', '.join(stopgap_hits)})")
        if hook_warning:
            notes.append(f"(hook-warning: {hook_warning})")
        return " ".join(notes)

    def _close_window(self, ts):
        closed = {
            "end_event_count": self.total_tool_event_count,
            "end_ts": ts,
            # PASS2 item 3: deterministic claim-presence over RECENT, computed
            # here (single windowing source) so observer.py and replay.py apply
            # the identical verify-claim class-(a) gate without drifting.
            "has_claim": contains_claim(self.recent_texts),
            "text": format_window(
                self.current_task,
                self.ledger_buffer,
                self.recent_texts,
                self.task_addressed,
                self.events_since_task,
                format_session_facts(self, self.total_tool_event_count),
            ),
        }
        self.ledger_buffer = []
        self.tool_event_count_since_window = 0
        self.last_window_ts = ts
        self.edits_with_prior_read_this_window = []
        return closed

    def _update_session_facts(self, name, input_, event_count):
        """DESIGN.md §2f: durable facts the classifier can cite past the
        ledger horizon — same regex tier as STOPGAP_MARKERS, no semantic
        verification of what a command actually covered."""
        if not isinstance(input_, dict):
            return
        cls = detect_verification_class(name, input_)
        if cls:
            self.last_verification[cls] = (event_count, tool_target(input_))
        path = input_.get("file_path") or input_.get("notebook_path")
        if not path:
            return
        if name == "Read":
            self.last_read_event[path] = event_count
        elif name in ("Edit", "Write", "MultiEdit", "NotebookEdit"):
            if path in self.last_read_event:
                self.edits_with_prior_read_this_window.append((path, self.last_read_event[path]))
            self.last_edit_event[path] = event_count

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
                    self.recent_texts.append(_clip(text, RECENT_MAX_CHARS))
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
                target = tool_target(input_)
                status = tool_result_status(c)
                stopgap_hits = detect_stopgap_markers(name, input_)
                result_text = _flatten_content_text(c.get("content"))
                hook_warning = extract_hook_warning(result_text)
                note = self._annotate_ledger(name, target, status, stopgap_hits, hook_warning)
                line = f"{label} {target} {status}".strip()
                if note:
                    line += f" {note}"
                self.ledger_buffer.append(line)
                self.tool_event_count_since_window += 1
                self.total_tool_event_count += 1
                self.events_since_task += 1
                self._update_session_facts(name, input_, self.total_tool_event_count)

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
                    if len(stripped) >= 8 and not stripped.startswith(HARNESS_TEXT_PREFIXES):
                        self.current_task = _clip(stripped, TASK_MAX_CHARS)
                        self.task_addressed = False
                        self.events_since_task = 0
                        # DESIGN.md §2f: every TASK-set event is, by this
                        # branch's own gate, a real human chat message — never
                        # a harness/hook text — so this is a user-ordered
                        # context switch by construction. Recorded durably so
                        # the classifier doesn't have to infer freshness from
                        # RECENT alone once this scrolls out of it.
                        self.context_switches.append((self.total_tool_event_count, _truncate(stripped, 60)))
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


# --- classifier latency stamp (synchronous-caller throttle detection) -------
# Keep in lockstep with valve.VERDICTS_DIR (same env override, same default);
# common can't import valve without inverting the dependency direction.
VERDICTS_DIR = os.environ.get("DAEMON_VERDICTS_DIR") or os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "verdicts"
)
CLASSIFIER_STAMP = os.path.join(VERDICTS_DIR, "classifier_latency.json")


def _write_classifier_stamp(duration_s, timed_out):
    """Record the latest classifier call's latency (any caller — observer or
    gate hooks). Latency is bimodal (~3s healthy, minutes under rate-limit
    saturation — see call_classifier's timeout comment), so one recent sample
    is a usable throttle signal for synchronous callers that can't afford the
    saturated mode. Best-effort: never raises."""
    try:
        os.makedirs(VERDICTS_DIR, exist_ok=True)
        tmp = f"{CLASSIFIER_STAMP}.{os.getpid()}.tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(
                {"ts": time.time(), "duration_s": round(duration_s, 2), "timed_out": timed_out},
                f,
            )
        os.replace(tmp, CLASSIFIER_STAMP)
    except OSError:
        pass


def classifier_throttled(fresh_s=600, slow_s=20):
    """True when the latest classifier call suggests the account is currently
    rate-limit saturated: stamp younger than fresh_s, and that call either
    timed out or ran slow_s or slower. Missing / stale / unreadable stamps all
    read as healthy — synchronous callers should try (their own call fails
    open anyway); the point here is only to skip a wait that recent evidence
    says is dead."""
    try:
        with open(CLASSIFIER_STAMP, encoding="utf-8") as f:
            d = json.load(f)
        if time.time() - float(d.get("ts", 0)) > fresh_s:
            return False
        return bool(d.get("timed_out")) or float(d.get("duration_s", 0)) >= slow_s
    except (OSError, ValueError, TypeError):
        return False


def call_classifier(system_prompt, window_text, model=MODEL, timeout=180, cwd=None):
    # timeout=180, not 60: classifier latency is bimodal — ~3s healthy, or
    # minutes when the account is rate-limit saturated (orchestrator fleets).
    # A 60s kill lands exactly between the modes: full wait, zero verdict,
    # retry into the same throttle (2026-07-04, ~16:00–17:00, all sessions).
    # The ceiling a verdict stays whisper-fresh is minutes, so wait it out.
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
    t0 = time.monotonic()
    try:
        proc = subprocess.run(
            cmd, capture_output=True, text=True, timeout=timeout, cwd=cwd or NEUTRAL_CWD
        )
    except subprocess.TimeoutExpired:
        _write_classifier_stamp(timeout, True)
        return {"error": "timeout"}
    except OSError as e:
        # spawn failure says nothing about API latency — leave the stamp alone
        return {"error": f"spawn failed: {e}"}
    _write_classifier_stamp(time.monotonic() - t0, False)
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


# "once" handled separately (fire-at-most-once); "advice-recur" re-arms the
# priming tier every N events so long runs get the advice back into context
# after it scrolls out (§2e, Peter 2026-07-05).
COOLDOWN_EVENTS = {"standard": 20, "slow": 40, "advice-recur": 300}


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
