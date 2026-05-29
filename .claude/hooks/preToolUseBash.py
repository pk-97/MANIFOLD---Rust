#!/usr/bin/env python3
"""
PreToolUse hook for Bash. Two jobs, evaluated in this order:

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
commit message `-m "fix the \`foo\` helper"`) and are NOT treated as
command substitutions. Only a substitution that would genuinely execute
(unescaped, outside single quotes) is pulled out and classified.

Fail-safe by construction: if the classifier is ever unsure, it does NOT
allow — it falls through to the deny check, and past that to the normal
permission flow (a prompt). The only way to reach "allow" is for every
parsed command-position to be a pre-approved head with no output redirect
outside /tmp and no mutating flag. A misparse costs at most one avoidable
prompt; it can never silently green-light an unapproved write.

Receives `{"tool_name": "Bash", "tool_input": {"command": "..."}}` on
stdin. Emits a JSON object with hookSpecificOutput.permissionDecision
("allow" or "deny") plus a reason, or nothing (normal flow).
"""
import json
import re
import sys


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
    """True if `cmd` starts with `cd <something> && ...` or `cd <something>; ...`."""
    return bool(re.match(r"\s*cd\s+\S+\s*(&&|;)", cmd))


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
# Deny messages (unchanged policy for non-pre-approved compounds)
# ---------------------------------------------------------------------------

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


def build_allow() -> dict:
    return {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": "Pre-approved command (read-only or git/cargo workflow; auto-approved by preToolUseBash hook).",
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

    # 1. Pre-approved? Allow outright, pipes and loops included.
    if is_preapproved_command(cmd):
        json.dump(build_allow(), sys.stdout)
        return 0

    # 2. Not pre-approved: enforce the no-pipe / no-cd-prefix rewrite policy.
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
