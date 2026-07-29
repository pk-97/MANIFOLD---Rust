#!/usr/bin/env python3
"""
PreToolUse hook for Bash. Three jobs, evaluated in this order:

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

Obsolete when: the harness gains native argument-level Bash permissioning (per-argument allow rules), making the classifier layer redundant.
"""
import json
import os
import re
import shlex
import subprocess
import sys
import traceback
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
    # `awk` is likewise excluded — system(), in-program `print > path`
    # writes, and `-f <file>` make it an interpreter, not a filter
    # (2026-07-26 audit: `awk 'BEGIN{system(...)}'` sailed through both
    # this list and the old `awk *` allow rules).
    "sort", "uniq", "cut", "tr", "jq", "column", "paste", "comm",
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

# cc-fleet subcommands safe to auto-allow: read-only inspection plus the
# durably-authorized K3 lane workflow (spawning/polling headless subagents —
# Peter's 2026-07-18 routing directive, approved in-session: K3-low via the
# `kimi` provider is the default lane agent, and prompt friction
# defeats the policy; spend is bounded by the Kimi membership plan).
# Provider mutation (add/edit/remove/import/default), key material (keyget),
# interactive/tmux modes (run/spawn/teardown/hide/show), and
# uninstall/update deliberately still prompt.
CC_FLEET_SUB = {
    "list", "models", "doctor", "ps", "subagent", "subagent-status",
    "subagent-gc", "refresh", "help", "completion",
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


def _top_level_segments(cmd: str):
    """Split `cmd` into top-level command segments (quote/escape/paren-aware).

    Yields (segment_text, paren_depth_at_start). Segments split on `;`, `&&`,
    `||`, `|`, and newlines at any depth, but the depth lets the caller treat
    subshell segments differently (a `(cd X && ...)` cwd change dies with the
    subshell)."""
    seg_start, depth, seg_depth = 0, 0, 0
    i, n = 0, len(cmd)
    quote = None
    while i < n:
        c = cmd[i]
        if quote:
            if c == "\\" and quote == '"':
                i += 2
                continue
            if c == quote:
                quote = None
            i += 1
            continue
        if c == "\\":
            i += 2
            continue
        if c in ("'", '"'):
            quote = c
            i += 1
            continue
        if c == "(":
            depth += 1
            i += 1
            continue
        if c == ")":
            depth = max(0, depth - 1)
            i += 1
            continue
        if c in (";", "\n") or (c in ("&", "|") and i + 1 < n and cmd[i + 1] == c) or c == "|":
            yield cmd[seg_start:i], seg_depth
            i += 2 if (c in ("&", "|") and i + 1 < n and cmd[i + 1] == c) else 1
            seg_start = i
            seg_depth = depth
            continue
        i += 1
    yield cmd[seg_start:], seg_depth


def persistent_cd_guard(cmd: str, cwd: str):
    """0g. Deny a top-level `cd` that parks the persistent shell cwd anywhere
    but a checkout root. cwd persists across Bash calls, so a stray `cd`
    surfaces as a DELAYED failure: the near-miss merge in
    SEMANTIC_WORKFLOW_PROGRAMS §10.5, then the real thing on 2026-07-27 — a
    no-op `cd` left the shell in a worktree and the landing merge silently
    merged a branch into itself. Correctness guard: runs in EVERY mode.

    Allowed targets (the recovery moves): the main checkout root, or a slot
    ring worktree root (`.claude/worktrees/<slot>`) — a lane returning to its
    own base. Subshell `(cd X && ...)` doesn't persist and is exempt.
    Everything else: use `git -C`, `--manifest-path`, or absolute paths.
    """
    if "cd" not in cmd:
        return None
    root = os.path.realpath(str(_PROJECT_DIR))
    worktrees = os.path.realpath(str(_WORKTREES_DIR))
    for seg, depth in _top_level_segments(cmd):
        seg = seg.strip()
        if depth > 0 or not re.match(r"cd(\s|$)", seg):
            continue
        rest = seg[2:].strip()
        try:
            toks = shlex.split(rest)
        except ValueError:
            toks = rest.split()
        target = next((t for t in toks if not re.match(r"^(-P|-L|-e|@?[0-9]*>>?|<)", t)), "")
        if target == "-":
            resolved = "<previous dir, unknowable statically>"
        elif not target:
            resolved = os.path.expanduser("~")
        else:
            resolved = os.path.realpath(
                os.path.join(cwd, os.path.expanduser(target)))
        if resolved == root:
            continue
        if os.path.dirname(resolved) == worktrees:
            continue
        return (
            f"Persistent `cd` to {resolved!r} denied: the shell cwd persists "
            "across Bash calls and a stray cd surfaces as a delayed failure "
            "(2026-07-27: a leftover cd made a landing merge silently no-op). "
            "Use `git -C <dir>`, `--manifest-path`, or absolute paths; `cd` "
            "back to the project root or your worktree slot root is allowed."
        )
    return None


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

    if head in ("cc-fleet", "ccf"):
        sub = next((t for t in toks[1:] if not t.startswith("-")), "")
        return sub in CC_FLEET_SUB

    if head == "sed":
        # `-i` / `--in-place` edits the file. Reject any short-flag cluster
        # containing `i`, or `--in-place`. (The script's `w` command is the
        # other write path — caught on the RAW command by sed_write_guard,
        # since quoted scripts are collapsed before this classifier runs.)
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


_PROJECT_DIR = Path(__file__).resolve().parents[2]
_WORKTREES_DIR = _PROJECT_DIR / ".claude" / "worktrees"


def _main_checkout_path():
    """Return the main repo checkout path, even from within a worktree.

    Worktrees live at <main>/.claude/worktrees/<slot>/. _PROJECT_DIR resolves
    to the worktree root; climbing to the grandparent of the worktrees dir
    yields the main checkout."""
    p = _PROJECT_DIR.resolve()
    if p.parent.name == "worktrees":
        return p.parent.parent.parent
    return p


_ORCH_VERDICTS_DIR = _main_checkout_path() / ".claude" / "orchestration" / "verdicts"


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
    "merge current origin/main into your branch, run `scripts/landing_gate.py` "
    "(touched-crate clippy + nextest, deny bans, UI flow gate, docs/design "
    "status, GPU proofs when GPU paths touched), `git merge --no-ff` into "
    "main, push — if rejected because someone landed first, repeat. The "
    "workspace sweep is NOT a landing item; it runs nightly via "
    "scripts/trunk_health.py. Twin-killers: never cherry-pick/re-commit "
    "content that already exists as commits on a live branch (merge it "
    "instead, so SHAs stay shared); never delete a branch until `git "
    "merge-base --is-ancestor <tip> origin/main` confirms its commits are on "
    "main."
)


def landing_protocol_guard(cmd, cwd):
    """Return (ask_reason, allow_context) for a git command in `cmd`.
    `ask_reason` is set — unconditionally, for a force-rewrite of main
    (branch -f main,
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
# Unverified compound landing-merge guard (.claude/GIT_TREE_DISCIPLINE.md §3b)
#
# Session 4340cb05: a `fetch && merge origin/main && merge --no-ff && push`
# compound landed a merge on another session's branch because HEAD changed
# between steps and was never re-verified. GIT_TREE_DISCIPLINE.md §3b: "Never
# run the landing `git merge --no-ff` inside a compound chain... Re-verify
# `git branch --show-current` immediately before the merge step, as its own
# command." Unlike the warn-only guards above, this one DENIES: a compound
# (2+ segments) in the main checkout where a landing merge (merge while on
# main) follows an earlier branch-mutating segment (checkout/switch/merge)
# with no branch-state re-verification segment in between. A single
# (non-compound) landing merge, or a compound where a verify segment
# intervenes, is unaffected (stays the existing allow+reminder path in
# `landing_protocol_guard`).
# ---------------------------------------------------------------------------

def _is_branch_verify_sub(sub, rest_toks):
    """True for `git branch --show-current` or `git rev-parse --abbrev-ref
    HEAD` — the two forms that actually re-read current branch state."""
    if sub == "branch":
        return "--show-current" in rest_toks
    if sub == "rev-parse":
        return "--abbrev-ref" in rest_toks and "HEAD" in rest_toks
    return False


def detect_unverified_compound_landing_merge(cmd, cwd):
    """TICKETS.md T6: deny (not just warn) a compound where a landing merge
    (merge while on main) follows an earlier branch-mutating segment
    (checkout/switch/merge) with no branch-state re-verification in between
    — HEAD can change between a shared checkout's compound steps
    (GIT_TREE_DISCIPLINE.md §3b, session 4340cb05). Never raises."""
    try:
        segments = _shlex_segments(cmd)
        if len(segments) < 2:
            return None  # not a compound; single landing merges are unaffected
        unverified_mutation = False
        for toks in segments:
            toks = _strip_leading_keywords(toks)
            if not toks or toks[0] != "git":
                continue
            target_dir, sub, rest = _git_checkout_dir(toks, cwd)
            if target_dir is None or not _in_main_checkout(target_dir):
                continue
            is_landing_merge_here = sub == "merge" and _current_branch(target_dir) == "main"
            if is_landing_merge_here and unverified_mutation:
                return ("This compound runs a landing merge after an earlier "
                        "branch-mutating step (checkout/switch/merge) with no "
                        "`git branch --show-current` in between — HEAD can change "
                        "between a shared checkout's compound steps "
                        "(GIT_TREE_DISCIPLINE.md §3b). Run the landing "
                        "`git merge --no-ff` as its OWN command, re-verifying "
                        "`git branch --show-current` immediately before it.")
            if _is_branch_verify_sub(sub, rest):
                unverified_mutation = False
            elif _is_branch_switch_sub(sub, rest):
                unverified_mutation = True
        return None
    except Exception:
        return None


# ---------------------------------------------------------------------------
# Worktree-ring guard (scripts/agent-worktree.py)
#
# 2026-07-15: 19 hand-rolled per-task worktrees × 15-60 GB cargo targets
# filled Peter's disk (455 GB). The pool is now a fixed ring of at most 6
# slots, and scripts/agent-worktree.py is the ONLY sanctioned way to get a
# worktree — its acquire path is structurally incapable of exceeding the
# cap. This guard closes the bypass: any `git worktree add` from an agent
# is denied, in EVERY permission mode (in auto/bypass modes there is no
# prompt to catch it otherwise). The script itself runs git as a
# subprocess, outside this hook's reach, so it is unaffected. `git
# worktree remove` is denied too — `remove --force` destroys uncommitted
# work, and raw removes bypass the slot ring's bookkeeping; releases go
# through the script (2026-07-26 audit: the old `git worktree *` allow
# rule made raw remove the zero-review path). prune/list stay allowed —
# cleanup shrinks the pool.
# ---------------------------------------------------------------------------

WORKTREE_ADD_REASON = (
    "`git worktree add` is denied — worktrees come ONLY from the slot ring: "
    "`scripts/agent-worktree.py acquire <task-label> <branch> "
    "[--tip REF]`. The ring caps the pool at 6 slots because hand-rolled "
    "worktrees filled the disk once (455 GB, 2026-07-15). If acquire says "
    "POOL FULL, surface that to Peter instead of working around it."
)

WORKTREE_REMOVE_REASON = (
    "`git worktree remove` is denied — releases go through the slot ring: "
    "`scripts/agent-worktree.py release <slot>`. Raw remove bypasses the "
    "ring's bookkeeping, and `--force` destroys uncommitted work."
)


def worktree_add_guard(cmd, cwd):
    """Return a deny reason if any segment is a `git worktree add` or
    `git worktree remove`, else None. Never raises — any failure yields
    None (normal flow)."""
    try:
        for toks in _shlex_segments(cmd):
            toks = _strip_leading_keywords(toks)
            if not toks or toks[0] != "git":
                continue
            _target_dir, sub, rest = _git_checkout_dir(toks, cwd)
            if sub == "worktree" and rest:
                if rest[0] == "add":
                    return WORKTREE_ADD_REASON
                if rest[0] == "remove":
                    return WORKTREE_REMOVE_REASON
        return None
    except Exception:
        return None


# ---------------------------------------------------------------------------
# Manifold GUI-launch guard (Peter 2026-07-29)
#
# Agents must never launch the full MANIFOLD GUI: it opens a real window on
# the performance machine and blocks the session forever (the app has no
# --version/--help — ANY unrecognized argv falls through to the winit
# event loop; a mistyped flag hung the lead session for 10 minutes today).
# Headless testing goes through the binary's headless subcommands or the
# ui-flows script driver. Denied in every mode. Obsolete when the app
# grows an argv parser that rejects unknown args instead of launching.
# ---------------------------------------------------------------------------

_MANIFOLD_HEADLESS_SUBCOMMANDS = {"ui-snap", "perf-soak", "rt-capture"}

MANIFOLD_GUI_REASON = (
    "Launching the full MANIFOLD GUI from an agent is denied — it opens a "
    "real window and never returns (any unrecognized argv falls through to "
    "the event loop; there is no --version/--help). Test headlessly: the "
    "binary's headless subcommands (ui-snap / perf-soak / rt-capture) or "
    "the ui-flows `--script` driver. Only Peter launches the GUI."
)


def manifold_gui_guard(cmd):
    """Return a deny reason if any segment runs the manifold app binary
    (directly or via `cargo run --bin manifold`) without one of the
    headless subcommands in its argv. Never raises."""
    try:
        for toks in _shlex_segments(cmd):
            toks = _strip_leading_keywords(toks)
            if not toks:
                continue
            head = toks[0]
            rest = None
            if head == "manifold" or (
                head.endswith("/manifold") and ("target/" in head or head == "./manifold")
            ):
                rest = toks[1:]
            elif head == "cargo" and "run" in toks and "--bin" in toks:
                bin_idx = toks.index("--bin")
                if bin_idx + 1 >= len(toks) or toks[bin_idx + 1] != "manifold":
                    continue
                rest = toks[toks.index("--") + 1:] if "--" in toks else []
            if rest is None:
                continue
            if not any(t in _MANIFOLD_HEADLESS_SUBCOMMANDS for t in rest):
                return MANIFOLD_GUI_REASON
        return None
    except Exception:
        return None


# ---------------------------------------------------------------------------
# Destructive outward-action guard (2026-07-26 permission audit)
#
# Settings allow rules skip the classifier, so any destructive action
# reachable through one needs a hook-level ask that fires regardless.
# Covers what the audit convicted: force-push to ANY ref (the landing
# guard only catches force-to-main; a force-push to a lane branch drops
# landed work the same way), remote branch deletion, `gh pr merge`
# (lands on origin outside the landing protocol — the verdict guards
# parse `git merge` syntax, not gh), and `bd delete` (permanently drops
# tracker state). Ask, not deny: legitimate cases exist, a human
# confirms them.
# ---------------------------------------------------------------------------


# sed's `w`/`W` script command writes files (`sed -n 'w /path' file`, `s/a/b/w
# path`) — invisible to the segment classifier because quoted scripts collapse
# to a placeholder before it runs, and the `Bash(sed -n *)` allow rule would
# otherwise auto-approve it (BUG-lu32 permission audit, 2026-07-28). Matches a
# w command at script start, after `;`/`{`, after an address, or as an s///w
# flag. False positives just prompt.
_SED_W_RE = re.compile(r"(?:^|[;{])\s*(?:[0-9$.,/*^\[\]-]+\s*)?[wW]\s|/[wW]\s")
_QUOTED_SPAN_RE = re.compile(r"'([^']*)'|\"([^\"]*)\"")


def sed_write_guard(cmd):
    """ASK when a sed command's script contains a file-writing w command."""
    if not re.search(r"(?:^|[|;&(\s])sed\s", cmd):
        return None
    for m in _QUOTED_SPAN_RE.finditer(cmd):
        span = (m.group(1) or m.group(2) or "") + " "
        if _SED_W_RE.search(span):
            return (
                "sed script contains a `w` (write-file) command — this writes "
                "to a path the allow rule never reviewed. If intended, approve; "
                "otherwise use an explicit redirect or the Write tool."
            )
    # unquoted script token (e.g. `sed -n w/tmp/x f` — no space needed after w)
    unquoted_w = re.compile(r"(?:^|[;{])\s*(?:[0-9$.,*^\[\]-]+\s*)?[wW][\s/]")
    for tok in cmd.split():
        if not tok.startswith("-") and unquoted_w.search(tok + " "):
            return (
                "sed script contains a `w` (write-file) command — this writes "
                "to a path the allow rule never reviewed. If intended, approve; "
                "otherwise use an explicit redirect or the Write tool."
            )
    return None


def destructive_outward_guard(cmd, cwd):
    """Return an ask reason for a force-push, remote branch deletion,
    `gh pr merge`, or `bd delete` in any segment; else None. Never
    raises."""
    try:
        for toks in _shlex_segments(cmd):
            toks = _strip_leading_keywords(toks)
            if not toks:
                continue
            if toks[0] == "git":
                _target_dir, sub, rest = _git_checkout_dir(toks, cwd)
                if sub == "push":
                    if _push_has_force_flag(rest):
                        return (
                            "Force-push. A force-push to any branch can drop "
                            "commits another session landed — the merge-trunk "
                            "model never rewrites published history "
                            "(.claude/GIT_TREE_DISCIPLINE.md §2)."
                        )
                    positional = [t for t in rest if not t.startswith("-")]
                    if "--delete" in rest or any(
                        t.startswith(":") for t in positional[1:]
                    ):
                        return (
                            "Remote branch deletion drops a shared branch and "
                            "its review context for every session — confirm "
                            "with Peter first."
                        )
            elif toks[0] == "gh" and len(toks) >= 3:
                if toks[1] == "pr" and toks[2] == "merge":
                    return (
                        "`gh pr merge` lands on origin outside the landing "
                        "protocol — verdict coverage and the merge-trunk "
                        "gate only run on local `git merge`. Land locally "
                        "per .claude/GIT_TREE_DISCIPLINE.md §2."
                    )
            elif toks[0] == "bd" and len(toks) >= 2 and toks[1] == "delete":
                return (
                    "`bd delete` permanently drops tracker state. `bd close` "
                    "keeps the record; delete is for mistakes only."
                )
        return None
    except Exception:
        return None


# ---------------------------------------------------------------------------
# Pre-land verdict-coverage guard (I1)
#
# When a `git merge` lands on main (current branch = main in the main
# checkout), every BUG- task id named on the merged branch's commits since
# origin/main must have a passing verdict (gate or no-gate) in the main
# checkout's verdict trail at .claude/orchestration/verdicts/<task>.jsonl.
#
# - Docs-only branches (all changed files under docs/) pass with a note.
# - Branches with no BUG- ids in the log pass (D6: pre-trail landings).
# - Violations deny with the missing tasks and the exact fix command.
# - Fails open: any error prints loudly and yields None (no guard).
# ---------------------------------------------------------------------------

_MERGE_OPT_VALUE = frozenset({
    "-m", "--message", "-F", "--file", "-e", "--edit", "--log", "--signoff",
    "-s", "--strategy", "-X", "--strategy-option",
})


def _get_merge_source_branch(rest_toks):
    """Extract the branch being merged from post-subcommand git merge tokens.

    Skips option-argument pairs so `-m "msg" branch` yields `branch`.
    Returns None if no positional remains."""
    skip_next = False
    positional = []
    for t in rest_toks:
        if skip_next:
            skip_next = False
            continue
        if t in _MERGE_OPT_VALUE:
            skip_next = True
            continue
        if t.startswith("-"):
            continue
        positional.append(t)
    return positional[0] if positional else None


def merge_verdict_guard(cmd, cwd):
    """Return (deny_reason, allow_context) for a git merge targeting main.

    I1: every BUG- task on the merged branch must have a passing verdict.
    Fails open on error — prints loudly, returns None."""
    try:
        for toks in _shlex_segments(cmd):
            toks = _strip_leading_keywords(toks)
            if not toks or toks[0] != "git":
                continue
            target_dir, sub, rest = _git_checkout_dir(toks, cwd)
            if target_dir is None or not _in_main_checkout(target_dir):
                continue
            if sub != "merge":
                continue
            if _current_branch(target_dir) != "main":
                continue

            # Git merge while on main — extract source branch
            source_branch = _get_merge_source_branch(rest)
            if source_branch is None:
                continue  # can't determine branch; skip guard

            # --- Docs-only check ---
            diff_r = subprocess.run(
                ["git", "-C", str(target_dir), "diff", "--name-only",
                 "origin/main", source_branch],
                capture_output=True, text=True, timeout=15,
            )
            if diff_r.returncode == 0:
                changed = [l.strip()
                           for l in diff_r.stdout.strip().split("\n") if l.strip()]
                if changed and all(f.startswith("docs/") for f in changed):
                    return (None,
                            "Docs-only merge: no verdict coverage required (D6).")

            # --- Extract BUG- ids from branch commits ---
            log_r = subprocess.run(
                ["git", "-C", str(target_dir), "log", "--format=%B",
                 f"origin/main..{source_branch}"],
                capture_output=True, text=True, timeout=15,
            )
            if log_r.returncode != 0:
                print(
                    f"merge_verdict_guard: git log failed "
                    f"(exit {log_r.returncode}): {log_r.stderr.strip()}",
                    file=sys.stderr,
                )
                return (None, None)

            bug_ids = re.findall(r"BUG-\w+", log_r.stdout)
            if not bug_ids:
                return (None,
                        "No BUG- task ids in merged branch commits. "
                        "Verdict coverage not required (D6: pre-trail landings).")

            # --- Check verdicts in MAIN checkout trail ---
            missing = []
            for bid in sorted(set(bug_ids)):
                vpath = _ORCH_VERDICTS_DIR / f"{bid}.jsonl"
                has_passing = False
                if vpath.exists():
                    with open(vpath) as f:
                        for line in f:
                            line = line.strip()
                            if not line:
                                continue
                            try:
                                v = json.loads(line)
                            except json.JSONDecodeError:
                                continue
                            if (v.get("schema") == 1
                                    and v.get("pass") is True
                                    and v.get("kind") in ("gate", "no-gate")):
                                has_passing = True
                                break
                if not has_passing:
                    missing.append(bid)

            if missing:
                fixes = "\n".join(
                    f'  gate_runner no-gate --task {m} '
                    f'--reason "<why-safe-without-gates>"'
                    for m in missing
                )
                return (
                    f"Merge blocked by I1: {len(missing)} task(s) lack passing "
                    f"verdicts:\n  {', '.join(missing)}\n\n"
                    f"Add no-gate verdicts:\n{fixes}\n\n"
                    f"Or run the design's declared gates (gate_runner per-lane) "
                    f"and retry the merge. (I1: every BUG- task on the merged "
                    f"branch needs verdict coverage.)",
                    None,
                )

            return (None,
                    "All BUG- tasks have passing verdicts. Merge permitted.")

    except Exception as e:
        print(f"merge_verdict_guard FAILED OPEN: {e}", file=sys.stderr)
        import traceback
        traceback.print_exc(file=sys.stderr)
        return (None, None)

    return (None, None)  # no merge-to-main segment found


# ---------------------------------------------------------------------------
# Pre-land flow-gate guard
#
# The flow gate (`scripts/run_ui_flows.py --touched ...`) was a prose line in
# the landing protocol: nine flows drifted red across multiple landings before
# a sweep caught them, because nothing made "the gate ran, green, at the tip
# that lands" a machine fact. run_ui_flows.py now writes a marker
# (.claude/orchestration/flow-gate-marker.json in the main checkout: HEAD sha,
# pass, ts) on every --touched run; this guard denies a merge into main when
# the merged branch touches flow-mapped paths (manifest path_triggers or flow
# JSON files) and no green marker exists for exactly that branch tip.
#
# Fails open loudly, like merge_verdict_guard. Docs-only and unmapped
# branches never see it.
#
# Obsolete when: landing moves to a server-side CI gate that runs the flow
# suite itself, or the flow suite is folded into gate_runner pre-land
# execution.
# ---------------------------------------------------------------------------

_FLOW_MARKER_PATH = (
    _main_checkout_path() / ".claude" / "orchestration" / "flow-gate-marker.json"
)
_FLOW_MANIFEST_PATH = (
    _main_checkout_path() / "scripts" / "ui-flows" / "manifest.json"
)


def _flow_mapped(path, triggers):
    """True when a changed path is covered by the flow gate: either a flow
    script itself or a path_triggers prefix match."""
    if (path.startswith("scripts/ui-flows/") and path.endswith(".json")
            and not path.endswith("manifest.json")):
        return True
    return any(path.startswith(prefix) for prefix in triggers)


def flow_gate_guard(cmd, cwd):
    """Return (deny_reason, allow_context) for a git merge targeting main.

    Deny when the merged branch touches flow-mapped paths and the flow-gate
    marker is missing, red, or written at a different HEAD than the branch
    tip. Fails open on error — prints loudly, returns (None, None)."""
    try:
        for toks in _shlex_segments(cmd):
            toks = _strip_leading_keywords(toks)
            if not toks or toks[0] != "git":
                continue
            target_dir, sub, rest = _git_checkout_dir(toks, cwd)
            if target_dir is None or not _in_main_checkout(target_dir):
                continue
            if sub != "merge":
                continue
            if _current_branch(target_dir) != "main":
                continue
            source_branch = _get_merge_source_branch(rest)
            if source_branch is None:
                continue

            diff_r = subprocess.run(
                ["git", "-C", str(target_dir), "diff", "--name-only",
                 "origin/main", source_branch],
                capture_output=True, text=True, timeout=15,
            )
            if diff_r.returncode != 0:
                return (None, None)
            changed = [l.strip()
                       for l in diff_r.stdout.strip().split("\n") if l.strip()]

            triggers = json.loads(
                _FLOW_MANIFEST_PATH.read_text()).get("path_triggers", {})
            mapped = [p for p in changed if _flow_mapped(p, triggers)]
            if not mapped:
                return (None, None)

            tip_r = subprocess.run(
                ["git", "-C", str(target_dir), "rev-parse", source_branch],
                capture_output=True, text=True, timeout=15,
            )
            if tip_r.returncode != 0:
                return (None, None)
            tip = tip_r.stdout.strip()

            problem = None
            if not _FLOW_MARKER_PATH.exists():
                problem = "no flow-gate marker exists"
            else:
                marker = json.loads(_FLOW_MARKER_PATH.read_text())
                if marker.get("head") != tip:
                    problem = (f"marker is stale: written at "
                               f"{str(marker.get('head'))[:12]}, branch tip is "
                               f"{tip[:12]}")
                elif marker.get("pass") is not True:
                    problem = "marker records a RED run"

            if problem is None:
                return (None,
                        f"Flow gate: green marker matches {source_branch} tip.")

            return (
                f"Merge blocked by the flow gate: {source_branch} touches "
                f"{len(mapped)} flow-mapped path(s) (e.g. {mapped[0]}) but "
                f"{problem}.\n\n"
                f"In the branch's worktree run:\n"
                f"  scripts/run_ui_flows.py --touched origin/main...HEAD\n"
                f"(writes the marker on completion), then retry the merge. "
                f"BUG-313 shipped exactly this way — its catching flow was "
                f"red and unrun.",
                None,
            )

    except Exception as e:
        print(f"flow_gate_guard FAILED OPEN: {e}", file=sys.stderr)
        import traceback
        traceback.print_exc(file=sys.stderr)
        return (None, None)

    return (None, None)  # no merge-to-main segment found


# ---------------------------------------------------------------------------
# Warning-only lints (never deny, never ask) — additionalContext on allow
#
# TICKETS.md T4/T5/T8: three independent shell-shape mistakes that have each
# burned a real session by making the model read back something other than
# what it thought it was reading. All three are computed unconditionally in
# `main()` and folded into the same additionalContext string the
# pre-approved-allow branch already builds; none of them ever change the
# allow/ask/deny decision.
# ---------------------------------------------------------------------------

def rg_replace_lint(cmd):
    """Warn (never deny) when an `rg`-headed segment carries -r/--replace —
    easily confused with -n (line numbers) / -l (filenames only). TICKETS.md
    T4: two sessions ran `rg -rn`/`rg -rl` meaning -n/-l and read back
    rewritten text as if it were real. Never raises."""
    try:
        for toks in _shlex_segments(cmd):
            toks = _strip_leading_keywords(toks)
            if not toks or toks[0] != "rg":
                continue
            for t in toks[1:]:
                if t in ("-r", "--replace") or t.startswith("--replace="):
                    return ("`-r`/`--replace` on `rg` REWRITES the matched text in "
                            "the output — it is not `-n` (line numbers) or `-l` "
                            "(filenames only). If you meant those, use `-n`/`-l`; "
                            "otherwise what you're about to read back has been "
                            "rewritten, not the real file contents.")
                if re.match(r"^-[A-Za-z]{2,}$", t) and "r" in t[1:]:
                    return ("Bundled short flag `%s` on `rg` includes `-r` "
                            "(--replace), which REWRITES matched text in the "
                            "output rather than showing line numbers/filenames. "
                            "If you meant `-n`/`-l`, use them un-bundled." % t)
        return None
    except Exception:
        return None


def workspace_sweep_lint(cmd):
    """Warn (never deny) on any workspace-wide cargo run — Peter 2026-07-23,
    after the RT panel-toggle change burned a full sweep on a 15-line edit:
    '90% of the time a full workspace test is not required and just wastes
    time and tokens.' Fires on `--workspace`/`--all` attached to cargo
    test/nextest/clippy/check/build in any segment. Never raises."""
    try:
        for toks in _shlex_segments(cmd):
            toks = _strip_leading_keywords(toks)
            # unwrap the build-lock wrapper: `bash .../with-build-lock.sh cargo ...`
            if len(toks) >= 2 and toks[0] == "bash" and toks[1].endswith("with-build-lock.sh"):
                toks = toks[2:]
            if not toks or toks[0] != "cargo":
                continue
            sub = next((t for t in toks[1:] if not t.startswith("-")), "")
            if sub not in ("test", "nextest", "clippy", "check", "build", "bench"):
                continue
            if any(t in ("--workspace", "--all") for t in toks):
                return (
                    "Workspace-wide cargo run detected. Peter's standing rule "
                    "(2026-07-23): ~90% of full sweeps are waste — scope to the "
                    "touched crates instead (`-p <crate>`, focused nextest "
                    "filter). A full workspace sweep is justified ONLY at a "
                    "multi-crate landing or when the blast radius genuinely "
                    "crosses crates — if that's now, say so in one line and "
                    "proceed; otherwise re-run scoped."
                )
        return None
    except Exception:
        return None


_TEST_RUNNER_RE = re.compile(r"^(?:cargo\s+(?:test|bench)\b|pytest\b|npm\s+(?:test|run)\b|go\s+test\b|swift\s+test\b)")
_FILTER_HEADS = {"rg", "grep", "egrep", "fgrep", "head", "tail"}


def _segments_with_ops(cmd):
    """Like `_shlex_segments` but also returns the operator immediately
    preceding each segment (None for the first segment). Needed because a
    masked-exit-status shape depends on WHICH operator joins two segments
    (`|` vs `;`), which `_shlex_segments` alone discards.

    Uses `shlex.shlex(..., punctuation_chars=True)` rather than plain
    `shlex.split` — a bare `shlex.split` does NOT separate an operator that's
    glued to an adjacent word with no space (`rg FAIL; echo` tokenizes as
    `'FAIL;'`, one token, not `'FAIL'` + `';'`), which is exactly the common
    shape here (`echo exit: $?` right after a `;` with no space). The
    punctuation-aware lexer still respects quoting correctly."""
    try:
        lex = shlex.shlex(cmd, posix=True, punctuation_chars=True)
        lex.whitespace_split = True
        tokens = list(lex)
    except ValueError:
        return []
    segments, current, preceding_op = [], [], None
    for t in tokens:
        if t in _SHELL_OPERATORS:
            if current:
                segments.append((current, preceding_op))
            current, preceding_op = [], t
        else:
            current.append(t)
    if current:
        segments.append((current, preceding_op))
    return segments


def masked_exit_status_lint(cmd):
    """Warn (never deny) when a test/build-runner segment is piped into a
    filter (rg/grep/head/tail) and the chain later echoes a status/`$?` —
    that status reflects the FILTER's exit code, not the test runner's.
    TICKETS.md T5: `cargo test | rg ...; echo exit: $?` reports rg's exit
    code; a background gate ending `| rg ...; echo GATE_DONE` looks like
    success unconditionally. Never raises."""
    try:
        segs = _segments_with_ops(cmd)
        for i, (toks, _op) in enumerate(segs):
            if not _TEST_RUNNER_RE.match(" ".join(toks)):
                continue
            if i + 1 >= len(segs):
                continue
            next_toks, next_op = segs[i + 1]
            if next_op != "|" or not next_toks or next_toks[0] not in _FILTER_HEADS:
                continue
            for later_toks, later_op in segs[i + 2:]:
                if later_op != ";":
                    continue
                if later_toks and (later_toks[0] == "echo" or any("$?" in t for t in later_toks)):
                    return ("This pipes a test/build command's output into a filter, "
                            "then echoes a status afterward — the echoed status/`$?` "
                            "reflects the FILTER's exit code, not the test runner's. "
                            "Use `${PIPESTATUS[0]}` or restructure with `&&`.")
        return None
    except Exception:
        return None


def trailing_comment_swallow_lint(cmd):
    """Warn (never deny) when a `#...` comment is followed, on the same
    line, by more text containing a shell operator — meaning a chained
    command got swallowed into the comment (bash `#` runs to end of line).
    TICKETS.md T8, session c9e4d45d: a self-grade append chained after a
    `#grep-ok` marker silently never ran. Reuses `sanitize()` so a `#`
    inside a quoted string doesn't count. Never raises."""
    try:
        structural, _inners = sanitize(cmd)
        for line in structural.split("\n"):
            idx = line.find("#")
            if idx == -1:
                continue
            after = line[idx + 1:]
            if not re.search(r"&&|\|\||;|(?<!\|)\|(?!\|)", after):
                continue
            return ("A `#...` comment swallows everything to end-of-line, including "
                    "the `&&`/`;`/`|` after it — the chained command "
                    "(%r) never runs. Put the comment last, or run the "
                    "swallowed command as its own call." % after.strip()[:60])
        return None
    except Exception:
        return None


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

    # 0b. Landing-protocol guard: a force-rewrite of main asks unconditionally;
    # a normal push/merge landing on main gets an allow + reminder below.
    landing_ask, landing_context = landing_protocol_guard(cmd, cwd)
    if landing_ask:
        json.dump(build_ask(landing_ask), sys.stdout)
        return 0

    # 0d. Worktree-ring guard: `git worktree add` is denied in every mode —
    # the slot ring (scripts/agent-worktree.py) is the only way to get a
    # worktree. Runs before the mode skip and the pre-approved allow.
    worktree_deny = worktree_add_guard(cmd, cwd)
    if worktree_deny:
        json.dump(build_deny([worktree_deny]), sys.stdout)
        return 0

    # 0g. Persistent-cwd guard: top-level `cd` off a checkout root is denied
    # in every mode — delayed-failure class, not prompt hygiene.
    cd_deny = persistent_cd_guard(cmd, cwd)
    if cd_deny:
        json.dump(build_deny([cd_deny]), sys.stdout)
        return 0

    # 0i. Manifold GUI-launch guard: running the app binary without a
    # headless subcommand is denied in every mode — it opens a real window
    # and never returns (Peter 2026-07-29).
    gui_deny = manifold_gui_guard(cmd)
    if gui_deny:
        json.dump(build_deny([gui_deny]), sys.stdout)
        return 0

    # 0c. Unverified compound landing-merge guard (T6): denies a compound
    # landing merge with no branch-state re-verification in between. Must
    # run before the pre-approved-allow branch below, or this compound would
    # sail through as a normal pre-approved git workflow.
    compound_deny_reason = detect_unverified_compound_landing_merge(cmd, cwd)
    if compound_deny_reason:
        json.dump(build_deny([compound_deny_reason]), sys.stdout)
        return 0

    # 0e. Pre-land verdict-coverage guard (I1): a merge into main requires
    # verdict coverage for every BUG- task on the merged branch. Denies with
    # the missing tasks and fix commands; passes with a note otherwise.
    merge_deny, merge_context = merge_verdict_guard(cmd, cwd)
    if merge_deny:
        json.dump(build_deny([merge_deny]), sys.stdout)
        return 0

    # 0e2. Pre-land flow-gate guard: a merge into main whose branch touches
    # flow-mapped paths requires a green flow-gate marker at the branch tip.
    flow_deny, flow_context = flow_gate_guard(cmd, cwd)
    if flow_deny:
        json.dump(build_deny([flow_deny]), sys.stdout)
        return 0

    # 0f. Destructive outward actions (force-push to any ref, remote branch
    # deletion, `gh pr merge`, `bd delete`) ASK in every mode — settings
    # allow rules would otherwise skip the classifier for them. Must run
    # before the pre-approved allow below.
    outward_ask = destructive_outward_guard(cmd, cwd)
    if outward_ask:
        json.dump(build_ask(outward_ask), sys.stdout)
        return 0

    # 0h. sed w-command guard (BUG-lu32 audit): a quoted sed script can write
    # files; the allow rule and the collapsed-span classifier both miss it.
    sed_ask = sed_write_guard(cmd)
    if sed_ask:
        json.dump(build_ask(sed_ask), sys.stdout)
        return 0

    # T4/T5/T8: warning-only lints, computed unconditionally so they land as
    # additionalContext on a pre-approved allow alongside the landing-protocol
    # context. Never affect the allow/ask/deny decision.
    rg_warning = rg_replace_lint(cmd)
    masked_exit_warning = masked_exit_status_lint(cmd)
    comment_swallow_warning = trailing_comment_swallow_lint(cmd)
    workspace_sweep_warning = workspace_sweep_lint(cmd)

    # 1. Pre-approved? Allow outright, pipes and loops included.
    if is_preapproved_command(cmd):
        combined = "\n\n".join(c for c in (
            landing_context, merge_context, flow_context,
            rg_warning, masked_exit_warning, comment_swallow_warning,
            workspace_sweep_warning,
        ) if c) or None
        json.dump(build_allow(combined), sys.stdout)
        return 0

    # 2. Not pre-approved: enforce the no-pipe / no-cd-prefix rewrite policy —
    # prompt hygiene only, so skipped in modes where Bash never prompts.
    if data.get("permission_mode") not in NON_PROMPTING_MODES:
        structural = sanitize(cmd)[0]
        reasons: list[str] = []
        if has_shell_pipe(structural):
            reasons.append(PIPE_REASON)
        if has_cd_prefix(cmd):
            reasons.append(CD_REASON)

        if reasons:
            json.dump(build_deny(reasons), sys.stdout)
            return 0

    # Workspace-sweep lint must reach the model even on NON-preapproved
    # commands (with-build-lock-wrapped cargo isn't preapproved) and in
    # non-prompting modes (overnight lanes are where the waste happens):
    # emit additionalContext WITHOUT a permissionDecision so the normal
    # permission flow still decides.
    if workspace_sweep_warning:
        json.dump({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": workspace_sweep_warning,
            }
        }, sys.stdout)

    return 0


if __name__ == "__main__":
    sys.exit(main())
