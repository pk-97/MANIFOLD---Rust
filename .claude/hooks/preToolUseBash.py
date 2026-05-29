#!/usr/bin/env python3
"""
PreToolUse hook for Bash. Two jobs, evaluated in this order:

  1. ALLOW read-only commands outright — even compound ones (pipes,
     `;` chains, `for`/`while` loops, `$(...)` substitutions) that the
     static `permissions.allow` matcher can't express, because that
     matcher only matches a command that *starts* with an allowlisted
     token (`rg`/`git`/`cargo`/...). A `for f in ...; do rg ...; done`
     or an `rg foo | head` reads to it as an unmatched compound and
     falls through to a manual approval prompt. This hook parses the
     whole command and, if EVERY command-position in it is a known
     read-only tool with no writes, returns permissionDecision="allow"
     — which is evaluated before the static matcher and before any
     prompt, so read-only research never stops to ask.

  2. DENY the leftovers that defeat the matcher AND aren't read-only:
     write-capable pipes and `cd <dir> && cmd` prefixes. The deny names
     the rewrite so the model fixes the call instead of forcing Peter
     to approve it by hand.

Fail-safe by construction: if the read-only classifier is ever unsure,
it does NOT allow — it falls through to the deny check, and past that to
the normal permission flow (a prompt). The only way to reach "allow" is
for every parsed command-position to be a known read-only head with no
output redirect outside /tmp and no mutating flag. A misparse costs at
most one avoidable prompt; it can never silently green-light a write.

Receives `{"tool_name": "Bash", "tool_input": {"command": "..."}}` on
stdin. Emits a JSON object with hookSpecificOutput.permissionDecision
("allow" or "deny") plus a reason, or nothing (normal flow).
"""
import json
import re
import sys


# ---------------------------------------------------------------------------
# Read-only vocabulary
# ---------------------------------------------------------------------------

# Command heads that only read state. If every command-position in a
# (possibly compound) invocation is one of these, the command is auto-allowed.
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

# cargo subcommands that only read metadata (no compile / run / fetch).
CARGO_READ_SUB = {
    "metadata", "tree", "verify-project", "locate-project", "pkgid",
}

# Shell keywords. `for`/`select`/`case`/`in`/`function` introduce a data
# list rather than a command, so a segment beginning with one of those is
# treated as read-only. The rest are stripped from the left of a segment
# until a real command head appears.
_DATA_KEYWORDS = {"for", "select", "case", "in", "function"}
_STRIP_KEYWORDS = {
    "if", "then", "elif", "else", "fi", "while", "until", "do", "done",
    "esac", "time", "!", "{", "}", "(", ")",
}


# ---------------------------------------------------------------------------
# Parsing helpers
# ---------------------------------------------------------------------------

def extract_substitutions(s: str):
    """
    Pull every `$(...)` and `` `...` `` command substitution out of `s`,
    returning (s_without_substitutions, [inner_command_strings]). Runs on
    the RAW command — before quote stripping — so a substitution hiding
    inside double quotes (e.g. `rg "$(rm -rf /)" f`) is still surfaced and
    classified, not erased. Handles nested `$(...)` via depth counting.
    """
    inners: list[str] = []
    out: list[str] = []
    i, n = 0, len(s)
    while i < n:
        if s[i] == "$" and i + 1 < n and s[i + 1] == "(":
            depth, j = 1, i + 2
            while j < n and depth > 0:
                if s[j] == "(":
                    depth += 1
                elif s[j] == ")":
                    depth -= 1
                j += 1
            inners.append(s[i + 2 : j - 1] if depth == 0 else s[i + 2 :])
            out.append(" ")
            i = j
        elif s[i] == "`":
            j = i + 1
            while j < n and s[j] != "`":
                j += 1
            inners.append(s[i + 1 : j])
            out.append(" ")
            i = j + 1
        else:
            out.append(s[i])
            i += 1
    return "".join(out), inners


def strip_quoted(cmd: str) -> str:
    """
    Remove the contents of single/double-quoted strings and heredoc bodies
    so a `|`/`>` inside a regex literal, jq query, or commit message doesn't
    false-positive as shell structure. Good enough for the structural checks
    we run; not a full shell tokenizer.
    """
    cmd = re.sub(
        r"<<-?['\"]?([A-Za-z_][A-Za-z0-9_]*)['\"]?\s*\n.*?^\1\s*$",
        "<<HEREDOC",
        cmd,
        flags=re.DOTALL | re.MULTILINE,
    )
    cmd = re.sub(r"'[^']*'", "''", cmd)
    cmd = re.sub(r'"[^"]*"', '""', cmd)
    return cmd


def has_shell_pipe(stripped: str) -> bool:
    """True if a `|` that isn't part of `||` is present."""
    return bool(re.search(r"(?<!\|)\|(?!\|)", stripped))


def has_cd_prefix(cmd: str) -> bool:
    """True if `cmd` starts with `cd <something> && ...` or `cd <something>; ...`."""
    return bool(re.match(r"\s*cd\s+\S+\s*(&&|;)", cmd))


def has_write_redirect(stripped: str) -> bool:
    """
    True if `stripped` contains an output redirect (`>`/`>>`) to anything
    other than /dev/null, a /tmp path, or a file-descriptor dup (`>&1`).
    Input redirects (`<`) are reads and ignored.
    """
    # Capture the target up to the next shell operator/whitespace only, so a
    # `2>/dev/null; cmd` doesn't grab `/dev/null;`. An fd-dup like `2>&1`
    # has `&` immediately after `>` (excluded from the target class) and so
    # produces no match — correctly treated as not-a-file-write.
    for m in re.finditer(r">>?\s*([^\s;|&<>()]+)", stripped):
        target = m.group(1)
        if target in ("/dev/null", "/tmp") or target.startswith("/tmp/"):
            continue
        return True
    return False


def split_segments(stripped: str):
    """Split a quote-stripped, substitution-free command into command-position
    segments on `|  ||  &&  ;  &` (background) and newlines. The single-`&`
    branch uses lookarounds so it does NOT split the `&` inside an fd-dup
    redirect like `2>&1` or `>&2` (preceded by `>`/`&` or followed by a
    digit) — only a genuine backgrounding/sequencing `&` separates."""
    parts = re.split(r"\|\||&&|[|;\n]|(?<![>&])&(?![&>0-9])", stripped)
    return [p.strip() for p in parts if p.strip()]


def segment_is_readonly(seg: str) -> bool:
    """Classify one command-position segment."""
    toks = seg.split()
    # Strip leading shell keywords and `VAR=value` env assignments. A `for`/
    # `case`/`in`/`function` keyword means the rest of the segment is a data
    # list, not a command — treat the whole segment as read-only.
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
        return sub in GIT_READ_SUB

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


def is_readonly_command(raw: str, _depth: int = 0) -> bool:
    """
    True iff the entire (possibly compound) command only reads state.
    Recurses one level into command substitutions.
    """
    if _depth > 4:
        return False  # pathological nesting — fail safe

    no_subst, inners = extract_substitutions(raw)

    # Every substitution must itself be read-only.
    for inner in inners:
        if not is_readonly_command(inner, _depth + 1):
            return False

    stripped = strip_quoted(no_subst)

    if has_write_redirect(stripped):
        return False

    segments = split_segments(stripped)
    if not segments:
        return False
    return all(segment_is_readonly(seg) for seg in segments)


# ---------------------------------------------------------------------------
# Deny messages (unchanged policy for write-capable compounds)
# ---------------------------------------------------------------------------

PIPE_REASON = (
    "Shell pipe (`|`) in a non-read-only command defeats Peter's Bash "
    "allowlist (matcher expects the call to start with `git`/`rg`/`cargo`/"
    "etc., not a compound). Read-only pipes are auto-allowed; this one isn't. "
    "Use the tool's native cap or split the write step into its own call."
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
            "permissionDecisionReason": "Read-only command (auto-approved by preToolUseBash hook).",
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

    # 1. Read-only? Allow outright, pipes and loops included.
    if is_readonly_command(cmd):
        json.dump(build_allow(), sys.stdout)
        return 0

    # 2. Not read-only: enforce the no-pipe / no-cd-prefix rewrite policy.
    stripped = strip_quoted(extract_substitutions(cmd)[0])
    reasons: list[str] = []
    if has_shell_pipe(stripped):
        reasons.append(PIPE_REASON)
    if has_cd_prefix(cmd):
        reasons.append(CD_REASON)

    if reasons:
        json.dump(build_deny(reasons), sys.stdout)

    return 0


if __name__ == "__main__":
    sys.exit(main())
