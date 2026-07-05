#!/usr/bin/env python3
"""
PreToolUse hook for Bash. Four jobs, evaluated in this order:

  0. WARN (allow + additionalContext, never an ask — Peter 2026-07-04,
     so automated orchestrations don't pause) on a branch-switch git
     command (checkout/switch/merge) that targets the MAIN checkout while
     another session's daemon pidfile (.claude/daemon/verdicts/*.pid) is
     live — see `shared_checkout_guard` and GIT_TREE_DISCIPLINE.md §1.
     Solo sessions and worktree-targeted commands (`git -C
     .claude/worktrees/...`) are unaffected; `checkout -- <paths>` (file
     restore, not a branch switch) is unaffected. Any failure in this
     check falls back to no-guard.

  0b. Landing-protocol guard (§1b): main is a merge-based trunk now, not a
     fast-forward pointer (GIT_TREE_DISCIPLINE.md §2 — the ff-only model
     produced twin commits under concurrent orchestrators, see the incident
     log). In the main checkout only: `git branch -f main ...` and any
     force-push targeting main ASK unconditionally (no foreign-session
     check — these are wrong regardless of concurrency, they drop commits
     under the merge model). A non-force push or merge that lands on main
     gets the normal allow with a short reminder of the landing protocol
     attached as additionalContext. See `landing_protocol_guard`.

  1. ALLOW pre-approved commands outright — even compound ones (pipes,
     `;` chains, `for`/`while` loops, `$(...)` substitutions) that the
     static `permissions.allow` matcher can't express, because that
     matcher only matches a command that *starts* with an allowlisted
     token (`rg`/`git`/`cargo`/...). A `for f in ...; do rg ...; done`,
     an `rg foo | head`, or a `git add . && git commit -q -m "..."`
     reads to it as an unmatched compound and falls through to a manual
     approval prompt. This hook parses the whole command and, if EVERY
     command-position is pre-approved, returns permissionDecision="allow"
     — evaluated before the static matcher and before any prompt.

     "Pre-approved" means either (a) a known read-only tool, or (b) a
     normal git/cargo workflow operation that CLAUDE.md durably authorizes
     ("commit and push when clean — don't ask"). Destructive git history /
     tree rewrites (reset, clean, rebase, gc, filter-branch) are NOT in the
     set — they still surface a prompt.

  2. DENY the leftovers that defeat the matcher AND aren't pre-approved:
     write-capable pipes and `cd <dir> && cmd` prefixes. The deny names
     the rewrite so the model fixes the call instead of forcing Peter to
     approve it by hand.

Parsing is escape/quote-aware (see `sanitize`): backticks and `$(...)`
that are escaped or sit inside single quotes are literal text (e.g. a
commit message `-m "fix the \\`foo\\` helper"`) and are NOT treated as
command substitutions. Only a substitution that would genuinely execute
(unescaped, outside single quotes) is pulled out and classified.

Fail-safe by construction: if the classifier is ever unsure, it does NOT
allow — it falls through to the deny check, and past that to the normal
permission flow (a prompt). The only way to reach "allow" is for every
parsed command-position to be a pre-approved head with no output redirect
outside /tmp and no mutating flag. A misparse costs at most one avoidable
prompt; it can never silently green-light an unapproved write.

Receives `{"tool_name": "Bash", "tool_input": {"command": "..."}, "session_id":
"...", "cwd": "..."}` on stdin. Emits a JSON object with hookSpecificOutput.
permissionDecision ("allow", "ask", or "deny") plus a reason, or nothing
(normal flow).
"""
import json
import os
import re
import shlex
import subprocess
import sys
from pathlib import Path


# ---------------------------------------------------------------------------
# Pre-approved vocabulary
# ---------------------------------------------------------------------------

# Command heads that only read state.
READ_ONLY = {
    # file / text inspection
    "cat", "head", "tail", "nl", "wc", "od", "xxd", "hexdump", "strings",
    "file", "stat", "less", "more", "tac", "rev",
    # search
    "rg", "grep", "egrep", "fgrep", "ag", "ack", "fd",
    # listing / paths
    "ls", "tree", "pwd", "dirname", "basename", "realpath", "readlink",
    # text processing (read-only). `tee` is deliberately excluded — it
    # writes to its file argument, which the redirect guard doesn't cover.
    "sort", "uniq", "cut", "tr", "awk", "jq", "column", "paste", "comm",
    "diff", "cmp", "fold", "expand", "unexpand", "seq",
    # code-shape
    "ast-grep", "sg",
    # hashing / encoding
    "md5", "md5sum", "shasum", "sha256sum", "cksum", "base64",
    # misc read-only
    "echo", "printf", "which", "type", "whoami", "date", "printenv",
    "true", "false", "test", "[", "uname", "hostname", "id", "groups",
    "read",  # shell builtin: reads stdin into a variable, writes no files
}

# git subcommands that only read repository state.
GIT_READ_SUB = {
    "log", "diff", "status", "show", "blame", "rev-parse", "ls-files",
    "ls-tree", "cat-file", "describe", "reflog", "shortlog", "grep",
    "rev-list", "merge-base", "for-each-ref", "name-rev", "whatchanged",
    "show-ref", "symbolic-ref", "var", "count-objects",
}

# git subcommands that write but are normal, durably-authorized workflow
# (CLAUDE.md: "commit and push when clean — don't ask"). Destructive history
# / tree rewrites — reset, clean, rebase, gc, filter-branch, prune — are
# deliberately EXCLUDED so they still surface a prompt.
GIT_WRITE_SUB = {
    "add", "commit", "push", "pull", "fetch", "stash", "switch", "restore",
    "checkout", "merge", "tag", "branch", "mv", "rm", "cherry-pick",
    "revert", "init", "remote", "config",
}

# cargo subcommands that only read metadata (no compile / run / fetch).
CARGO_READ_SUB = {
    "metadata", "tree", "verify-project", "locate-project", "pkgid",
}

# Shell keywords. `for`/`select`/`case`/`in`/`function` introduce a data
# list rather than a command, so a segment beginning with one of those is
# pre-approved. The rest are stripped from the left of a segment until a
# real command head appears.
_DATA_KEYWORDS = {"for", "select", "case", "in", "function"}
_STRIP_KEYWORDS = {
    "if", "then", "elif", "else", "fi", "while", "until", "do", "done",
    "esac", "time", "!", "{", "}", "(", ")",
}

# Placeholder a quoted span collapses to. Deliberately not a /tmp path and
# not a known command head, so a *quoted* redirect target still reads as a
# write (falls through to a prompt) and a quoted leading word isn't mistaken
# for an approved command.
_QUOTED = "\x01Q\x01"


# ---------------------------------------------------------------------------
# Escape/quote-aware scanner
# ---------------------------------------------------------------------------

def _read_paren(s: str, i: int):
    """`s[i:]` starts with `$(`. Return (inner_body, index_past_close)."""
    depth, j, n = 1, i + 2, len(s)
    while j < n and depth > 0:
        if s[j] == "\\" and j + 1 < n:
            j += 2
            continue
        if s[j] == "(":
            depth += 1
        elif s[j] == ")":
            depth -= 1
        j += 1
    return (s[i + 2 : j - 1] if depth == 0 else s[i + 2 :]), j


def _read_backtick(s: str, i: int):
    """`s[i]` is a backtick. Return (inner_body, index_past_close)."""
    j, n = i + 1, len(s)
    while j < n and s[j] != "`":
        j += 2 if (s[j] == "\\" and j + 1 < n) else 1
    return s[i + 1 : j], (j + 1 if j < n else j)


def sanitize(s: str):
    """
    Single pass, escape/quote-aware. Returns (structural, inners):
      - structural: the command with quoted spans / heredoc bodies collapsed
        to a neutral placeholder and command substitutions removed, leaving
        only real shell structure (operators, unquoted words) for segment
        analysis.
      - inners: command-substitution bodies that would ACTUALLY execute
        (unescaped `$(...)` / `` `...` `` outside single quotes), to be
        classified recursively.
    Backticks / `$()` inside single quotes, or escaped (`\\$`, `` \\` ``),
    are literal text — neither structural nor extracted.
    """
    inners: list[str] = []
    out: list[str] = []
    i, n = 0, len(s)
    while i < n:
        c = s[i]

        if c == "\\" and i + 1 < n:
            i += 2  # escaped char — literal, no structural meaning
            continue

        if c == "'":  # single-quoted: fully literal
            j = i + 1
            while j < n and s[j] != "'":
                j += 1
            out.append(_QUOTED)
            i = j + 1
            continue

        if c == '"':  # double-quoted: collapse, but extract active substitutions
            out.append(_QUOTED)
            j = i + 1
            while j < n and s[j] != '"':
                if s[j] == "\\" and j + 1 < n:
                    j += 2
                    continue
                if s[j] == "$" and j + 1 < n and s[j + 1] == "(":
                    inner, j = _read_paren(s, j)
                    inners.append(inner)
                    continue
                if s[j] == "`":
                    inner, j = _read_backtick(s, j)
                    inners.append(inner)
                    continue
                j += 1
            i = j + 1
            continue

        if c == "<" and i + 1 < n and s[i + 1] == "<":  # heredoc
            m = re.match(r"<<-?\s*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\1", s[i:])
            if m:
                delim = m.group(2)
                end = re.compile(r"\n[ \t]*" + re.escape(delim) + r"[ \t]*(?:\n|$)")
                em = end.search(s, i + m.end())
                out.append(_QUOTED)
                i = em.end() if em else n
                continue
            out.append(c)
            i += 1
            continue

        if c == "$" and i + 1 < n and s[i + 1] == "(":  # active subst (unquoted)
            inner, i = _read_paren(s, i)
            inners.append(inner)
            out.append(" ")
            continue

        if c == "`":  # active subst (unquoted)
            inner, i = _read_backtick(s, i)
            inners.append(inner)
            out.append(" ")
            continue

        out.append(c)
        i += 1

    return "".join(out), inners


# ---------------------------------------------------------------------------
# Structural checks (run on the sanitized string)
# ---------------------------------------------------------------------------

def has_shell_pipe(structural: str) -> bool:
    """True if a `|` that isn't part of `||` is present."""
    return bool(re.search(r"(?<!\|)\|(?!\|)", structural))


def has_cd_prefix(cmd: str) -> bool:
    """True if `cmd` starts with `cd <something> && ...` or `cd <something>; ...`.

    The target may be quoted (`cd "MANIFOLD - Rust" && ...`) or carry escaped
    spaces (`cd MANIFOLD\\ -\\ Rust && ...`). A bare `\\S+` stops at the first
    space inside the path and misses the prefix entirely — so the command
    silently falls through to a manual prompt instead of this helpful deny.
    Match the three target forms: double-quoted, single-quoted, or an
    unquoted run that allows backslash-escaped chars."""
    return bool(re.match(
        r"""\s*cd\s+(?:"[^"]*"|'[^']*'|(?:\\.|\S)+)\s*(&&|;)""", cmd))


def has_write_redirect(structural: str) -> bool:
    """
    True if `structural` contains an output redirect (`>`/`>>`) to anything
    other than /dev/null or a /tmp path. A quoted target collapses to the
    placeholder (not /tmp) and so reads as a write. An fd-dup like `2>&1`
    has `&` immediately after `>` (excluded from the target class) and so
    produces no match — correctly treated as not-a-file-write.
    """
    for m in re.finditer(r">>?\s*([^\s;|&<>()]+)", structural):
        target = m.group(1)
        if target in ("/dev/null", "/tmp") or target.startswith("/tmp/"):
            continue
        return True
    return False


def split_segments(structural: str):
    """Split the sanitized command into command-position segments on
    `|  ||  &&  ;  &` (background) and newlines. The single-`&` branch uses
    lookarounds so it does NOT split the `&` inside an fd-dup redirect like
    `2>&1` or `>&2` — only a genuine backgrounding/sequencing `&` separates."""
    parts = re.split(r"\|\||&&|[|;\n]|(?<![>&])&(?![&>0-9])", structural)
    return [p.strip() for p in parts if p.strip()]


def segment_is_allowed(seg: str) -> bool:
    """Classify one command-position segment as pre-approved or not."""
    toks = seg.split()
    # Strip leading shell keywords and `VAR=value` env assignments. A `for`/
    # `case`/`in`/`function` keyword means the rest of the segment is a data
    # list, not a command — pre-approve the whole segment.
    while toks:
        t = toks[0]
        if t in _DATA_KEYWORDS:
            return True
        if t in _STRIP_KEYWORDS:
            toks = toks[1:]
            continue
        if re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", t):
            toks = toks[1:]
            continue
        break
    if not toks:
        return True

    head = toks[0]

    if head == "git":
        # Skip global options (`-C path`, `-c k=v`, `--no-pager`) to find the
        # subcommand.
        i = 1
        while i < len(toks) and toks[i].startswith("-"):
            i += 2 if toks[i] in ("-C", "-c") else 1
        sub = toks[i] if i < len(toks) else ""
        return sub in GIT_READ_SUB or sub in GIT_WRITE_SUB

    if head == "cargo":
        i = 1
        while i < len(toks) and toks[i].startswith("+"):  # +toolchain
            i += 1
        sub = toks[i] if i < len(toks) else ""
        return sub in CARGO_READ_SUB

    if head == "sed":
        # `-i` / `--in-place` edits the file. Reject any short-flag cluster
        # containing `i`, or `--in-place`.
        for t in toks[1:]:
            if t.startswith("--in-place") or re.match(r"^-[A-Za-z]*i", t):
                return False
        return True

    if head == "find":
        bad = {"-delete", "-exec", "-execdir", "-ok", "-okdir",
               "-fprint", "-fprintf", "-fls"}
        return not any(t in bad for t in toks)

    return head in READ_ONLY


def is_preapproved_command(raw: str, _depth: int = 0) -> bool:
    """
    True iff the entire (possibly compound) command is pre-approved — every
    command-position is a read-only tool or a normal git/cargo workflow op,
    with no output redirect to a repo path. Recurses one level into command
    substitutions.
    """
    if _depth > 4:
        return False  # pathological nesting — fail safe

    structural, inners = sanitize(raw)

    # Every substitution that would actually execute must itself be approved.
    for inner in inners:
        if not is_preapproved_command(inner, _depth + 1):
            return False

    if has_write_redirect(structural):
        return False

    segments = split_segments(structural)
    if not segments:
        return False
    return all(segment_is_allowed(seg) for seg in segments)


# ---------------------------------------------------------------------------
# Shared-checkout guard (.claude/GIT_TREE_DISCIPLINE.md §1)
#
# Two live sessions, one main-checkout HEAD: a branch switch/merge in the main
# tree while another session's daemon is alive can silently move the tree out
# from under it (incident: commit 88257631 — a fast-forward merge resurrected
# a moved file's old path mid-rename). This guard does not add a new deny; it
# turns a branch-switch command that targets the main checkout, while another
# session's daemon pidfile is alive, into an "ask" so Peter is prompted by
# name instead of the switch happening silently. Any exception anywhere in
# this section falls back to no-guard (today's behavior) — never to blocking.
# ---------------------------------------------------------------------------

_PROJECT_DIR = Path(__file__).resolve().parents[2]
_WORKTREES_DIR = _PROJECT_DIR / ".claude" / "worktrees"
_VERDICTS_DIR = _PROJECT_DIR / ".claude" / "daemon" / "verdicts"


def find_live_foreign_session(own_session_id):
    """First session id under verdicts/*.pid that isn't `own_session_id` and
    whose pid passes a signal-0 liveness check. Malformed/dead pidfiles are
    skipped (read as absent), never treated as an error."""
    try:
        if not _VERDICTS_DIR.is_dir():
            return None
        for pid_file in sorted(_VERDICTS_DIR.glob("*.pid")):
            sid = pid_file.stem
            if sid == own_session_id:
                continue
            try:
                pid = int(pid_file.read_text().strip())
            except (OSError, ValueError):
                continue  # malformed pidfile -> treat as absent
            try:
                os.kill(pid, 0)
            except OSError:
                continue  # dead pid -> treat as absent
            return sid
    except Exception:
        return None
    return None


def _git_checkout_dir(toks, cwd):
    """Resolve the effective working dir for a `git [-C dir]... <sub>` segment,
    applying `-C` cumulatively (git semantics: each is relative to the last).
    Returns (resolved_dir, sub, rest_toks) or (None, None, None) if unparsable."""
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


def _is_branch_switch_sub(sub, rest_toks):
    """switch/merge always count; `checkout` counts unless it's the
    `checkout -- <paths>` file-restore form (destructive-to-worktree, not a
    branch switch — left alone per spec)."""
    if sub in ("switch", "merge"):
        return True
    if sub == "checkout":
        return "--" not in rest_toks
    return False


def _strip_leading_keywords(toks):
    while toks:
        t = toks[0]
        if t in _DATA_KEYWORDS:
            return []  # data list, not a command
        if t in _STRIP_KEYWORDS:
            toks = toks[1:]
            continue
        if re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", t):
            toks = toks[1:]
            continue
        break
    return toks


_SHELL_OPERATORS = {"&&", "||", ";", "|", "&"}


def _shlex_segments(cmd):
    """Tokenize `cmd` with real quote-unescaping (unlike `sanitize`, which
    collapses quoted spans to a placeholder — fine for the allow/deny
    classifier, which never needs the literal text, but wrong here: a `-C
    "<path>"` argument must survive with its real value, notably because
    the repo path itself contains a space ("MANIFOLD - Rust"). Splits the
    resulting token stream into command-position segments on operator
    tokens. Malformed quoting (`shlex.split` raising) yields no segments —
    fail-safe, same as everywhere else in this guard."""
    try:
        tokens = shlex.split(cmd, posix=True)
    except ValueError:
        return []
    segments = []
    current = []
    for t in tokens:
        if t in _SHELL_OPERATORS:
            if current:
                segments.append(current)
            current = []
        else:
            current.append(t)
    if current:
        segments.append(current)
    return segments


def shared_checkout_guard(cmd, session_id, cwd):
    """Return a warning string if `cmd` contains a branch-switch git
    command targeting the main checkout while another session's daemon is
    live; otherwise None. Delivered as additionalContext on an allow —
    NOT an ask — so automated orchestrations never pause on it (Peter,
    2026-07-04). Never raises — any failure yields None (no guard)."""
    try:
        for toks in _shlex_segments(cmd):
            toks = _strip_leading_keywords(toks)
            if not toks or toks[0] != "git":
                continue
            target_dir, sub, rest = _git_checkout_dir(toks, cwd)
            if target_dir is None or not _is_branch_switch_sub(sub, rest):
                continue
            try:
                resolved = target_dir.resolve()
            except OSError:
                continue
            in_main = resolved == _PROJECT_DIR or _PROJECT_DIR in resolved.parents
            in_worktrees = resolved == _WORKTREES_DIR or _WORKTREES_DIR in resolved.parents
            if not in_main or in_worktrees:
                continue
            foreign = find_live_foreign_session(session_id)
            if foreign:
                return (
                    f"Heads-up: branch-switch in the shared main checkout "
                    f"(`{' '.join(toks)}`) while session {foreign}'s daemon "
                    f"is live. This moves the tree under that session — "
                    f"proceed only if intended, prefer a worktree for branch "
                    f"work, and re-read branch state from command output "
                    f"afterwards (incident 88257631 / "
                    f"GIT_TREE_DISCIPLINE.md §1)."
                )
        return None
    except Exception:
        return None


# ---------------------------------------------------------------------------
# Landing-protocol guard (.claude/GIT_TREE_DISCIPLINE.md §1b / §2)
#
# The ff-only "main = last-known-good pointer" model (old §2) assumed one
# integrator lands at a time. Under concurrent orchestrator sessions a clean
# fast-forward was never actually possible, so every finishing session
# improvised its own landing — producing twin commits (same content, two
# lineages, different SHAs; see the incident log in GIT_TREE_DISCIPLINE.md
# and the `git-landing-protocol` memory). Main is now a merge-based trunk:
# land via fetch -> merge origin/main -> gate -> merge --no-ff -> push. This
# guard (a) unconditionally asks before a force-rewrite of main, since that's
# simply wrong now, not just concurrency-unsafe, and (b) attaches a
# deterministic reminder of the protocol to an otherwise-normal push/merge
# that lands on main. Scoped to the main checkout only, same as §1.
# ---------------------------------------------------------------------------

_MAIN_REF_TOKENS = ("main", "refs/heads/main")
_FORCE_PUSH_FLAGS_EXACT = {"--force", "-f", "--force-if-includes"}


def _current_branch(cwd):
    """Best-effort current branch name in `cwd`, or None on any failure."""
    try:
        out = subprocess.run(
            ["git", "-C", str(cwd), "rev-parse", "--abbrev-ref", "HEAD"],
            capture_output=True,
            text=True,
            timeout=3,
        )
        if out.returncode == 0:
            return out.stdout.strip()
    except Exception:
        pass
    return None


def _in_main_checkout(target_dir):
    try:
        resolved = target_dir.resolve()
    except OSError:
        return False
    in_main = resolved == _PROJECT_DIR or _PROJECT_DIR in resolved.parents
    in_worktrees = resolved == _WORKTREES_DIR or _WORKTREES_DIR in resolved.parents
    return in_main and not in_worktrees


def _push_targets_main(rest_toks, target_dir):
    """True if a `git push ...` with these post-subcommand tokens lands on
    main — either an explicit refspec naming main, or no refspec at all
    (0 or 1 positional args: bare push / push-with-remote-only), in which
    case it depends on the current branch."""
    positional = [t for t in rest_toks if not t.startswith("-")]
    if len(positional) >= 2:
        refspec = positional[-1]
        remote_part = refspec.split(":", 1)[-1] if ":" in refspec else refspec
        return remote_part in _MAIN_REF_TOKENS
    return _current_branch(target_dir) == "main"


def _push_has_force_flag(rest_toks):
    for t in rest_toks:
        if t in _FORCE_PUSH_FLAGS_EXACT or t.startswith("--force-with-lease"):
            return True
    return False


def _branch_force_targets_main(rest_toks):
    """True for `git branch -f/-F/--force main ...` (force-moves main)."""
    has_force = any(t in ("-f", "-F", "--force") for t in rest_toks)
    if not has_force:
        return False
    positional = [t for t in rest_toks if not t.startswith("-")]
    return bool(positional) and positional[0] == "main"


LANDING_PROTOCOL_REMINDER = (
    "Landing on main. Protocol (.claude/GIT_TREE_DISCIPLINE.md §2): fetch, "
    "merge current origin/main into your branch, rerun the gate (clippy + "
    "focused tests, full workspace sweep if blast radius says so), `git merge "
    "--no-ff` into main, push — if rejected because someone landed first, "
    "repeat. Twin-killers: never cherry-pick/re-commit content that already "
    "exists as commits on a live branch (merge it instead, so SHAs stay "
    "shared); never delete a branch until `git merge-base --is-ancestor <tip> "
    "origin/main` confirms its commits are on main."
)


def landing_protocol_guard(cmd, cwd):
    """Return (ask_reason, allow_context) for a git command in `cmd`.
    `ask_reason` is set — unconditionally, no foreign-session check, unlike
    `shared_checkout_guard` — for a force-rewrite of main (branch -f main,
    or a force-push landing on main): wrong under the merge-trunk model
    regardless of concurrency. `allow_context` is a landing-protocol
    reminder for an otherwise-normal non-force push/merge that lands on
    main. At most one of the two is ever set. Never raises; any failure
    yields (None, None)."""
    try:
        for toks in _shlex_segments(cmd):
            toks = _strip_leading_keywords(toks)
            if not toks or toks[0] != "git":
                continue
            target_dir, sub, rest = _git_checkout_dir(toks, cwd)
            if target_dir is None or not _in_main_checkout(target_dir):
                continue

            if sub == "branch" and _branch_force_targets_main(rest):
                return (
                    "`git branch -f main ...` force-moves the main pointer. "
                    "Main is a merge-based trunk now, not a fast-forward "
                    "target (.claude/GIT_TREE_DISCIPLINE.md §2) — this "
                    "can drop commits that aren't ancestors of <tip>. Land "
                    "via the merge protocol instead.",
                    None,
                )

            if sub == "push":
                if _push_has_force_flag(rest) and _push_targets_main(rest, target_dir):
                    return (
                        "Force-push targeting main. Main is a merge-based "
                        "trunk now (.claude/GIT_TREE_DISCIPLINE.md §2) — "
                        "a force-push can drop commits another session "
                        "landed. Use the merge protocol (fetch, merge "
                        "origin/main, gate, merge --no-ff, push) instead.",
                        None,
                    )
                if _push_targets_main(rest, target_dir):
                    return None, LANDING_PROTOCOL_REMINDER

            if sub == "merge" and _current_branch(target_dir) == "main":
                return None, LANDING_PROTOCOL_REMINDER

        return None, None
    except Exception:
        return None, None


# ---------------------------------------------------------------------------
# Deny messages (unchanged policy for non-pre-approved compounds)
# ---------------------------------------------------------------------------

# Permission modes in which Bash calls never prompt. The pipe/cd-prefix
# denies exist purely as prompt hygiene (a compound defeats the allowlist
# matcher, and in default mode every miss becomes a prompt); in these modes
# the deny protects nothing and only costs a rewrite round-trip, so main()
# skips it. The git guards (shared-checkout, landing-protocol) stay active
# in every mode — they guard correctness, not prompts. A missing/unknown
# permission_mode keeps the deny (safe default).
NON_PROMPTING_MODES = frozenset({"auto", "bypassPermissions"})

PIPE_REASON = (
    "Shell pipe (`|`) in a non-pre-approved command defeats Peter's Bash "
    "allowlist (matcher expects the call to start with `git`/`rg`/`cargo`/"
    "etc., not a compound). Read-only and git/cargo-workflow pipes are "
    "auto-allowed; this one isn't. Use the tool's native cap or split the "
    "write step into its own call."
)

CD_REASON = (
    "`cd <dir> && cmd` prefix bypasses the allowlist. cwd is already the "
    "project root. For a different cargo target use `--manifest-path`; "
    "otherwise run a dedicated Bash call without the `cd &&` chain."
)


def build_allow(additional_context: str | None = None) -> dict:
    out = {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": "Pre-approved command (read-only or git/cargo workflow; auto-approved by preToolUseBash hook).",
        }
    }
    if additional_context:
        out["hookSpecificOutput"]["additionalContext"] = additional_context
    return out


def build_ask(reason: str) -> dict:
    return {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "ask",
            "permissionDecisionReason": reason,
        }
    }


def build_deny(reasons: list[str]) -> dict:
    return {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": " ".join(reasons) + " Retry with the fixed command.",
        }
    }


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except json.JSONDecodeError:
        return 0  # plumbing broke — let normal flow handle it

    cmd = data.get("tool_input", {}).get("command", "")
    if not cmd:
        return 0

    cwd = data.get("cwd") or os.getcwd()

    # 0. Shared-checkout guard: a branch switch in the main tree while
    # another session's daemon is live gets a warning attached as context —
    # never an ask, so orchestrations don't pause (Peter, 2026-07-04).
    shared_checkout_context = shared_checkout_guard(cmd, data.get("session_id"), cwd)

    # 0b. Landing-protocol guard: a force-rewrite of main asks unconditionally;
    # a normal push/merge landing on main gets an allow + reminder below.
    landing_ask, landing_context = landing_protocol_guard(cmd, cwd)
    if landing_ask:
        json.dump(build_ask(landing_ask), sys.stdout)
        return 0

    # 1. Pre-approved? Allow outright, pipes and loops included.
    if is_preapproved_command(cmd):
        combined = "\n\n".join(c for c in (shared_checkout_context, landing_context) if c) or None
        json.dump(build_allow(combined), sys.stdout)
        return 0

    # 2. Not pre-approved: enforce the no-pipe / no-cd-prefix rewrite policy —
    # prompt hygiene only, so skipped in modes where Bash never prompts.
    if data.get("permission_mode") in NON_PROMPTING_MODES:
        return 0

    structural = sanitize(cmd)[0]
    reasons: list[str] = []
    if has_shell_pipe(structural):
        reasons.append(PIPE_REASON)
    if has_cd_prefix(cmd):
        reasons.append(CD_REASON)

    if reasons:
        json.dump(build_deny(reasons), sys.stdout)

    return 0


if __name__ == "__main__":
    sys.exit(main())
