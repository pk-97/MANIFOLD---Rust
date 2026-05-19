#!/usr/bin/env python3
"""
PreToolUse hook for Bash. Catches commands that defeat Peter's
allowlist matcher — pipes and `cd <dir> && cmd` prefixes — and
denies with a message that names the rewrite, so the model fixes
the call instead of forcing Peter to manually approve every
read-only research step.

CLAUDE.md's "Shell — no pipes" / "Shell — no cd prefix" hard rules
state the policy; this hook enforces it. A CLAUDE.md sentence is a
hint, a hook is a wall.

Receives `{"tool_name": "Bash", "tool_input": {"command": "..."}}`
on stdin. Returns either nothing (allow) or a JSON object with
hookSpecificOutput.permissionDecision = "deny" + reason (block).
"""
import json
import re
import sys


def strip_quoted(cmd: str) -> str:
    """
    Remove the contents of single- and double-quoted strings, plus
    heredoc bodies, so a `|` inside a jq query, a regex literal, or
    a commit-message heredoc doesn't false-positive as a shell pipe.
    Doesn't try to handle escaped quotes perfectly — good enough for
    catching the structural pipes/redirects we care about.
    """
    # Heredoc: <<DELIM ... DELIM (or <<'DELIM' / <<"DELIM" / <<-DELIM).
    # Replace the entire delimited block with a placeholder so any
    # `|` inside (e.g. inside a commit-message documentation string)
    # is removed from structural consideration.
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
    """
    True if `stripped` contains a `|` that isn't part of `||` (logical
    OR). Negative lookbehind/ahead handle the boundary cases.
    """
    return bool(re.search(r"(?<!\|)\|(?!\|)", stripped))


def has_cd_prefix(cmd: str) -> bool:
    """
    True if `cmd` starts with `cd <something> && ...` or `cd <something>; ...`.
    """
    return bool(re.match(r"\s*cd\s+\S+\s*(&&|;)", cmd))


def build_deny(reasons: list[str]) -> dict:
    return {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": " ".join(reasons)
            + " Retry with the fixed command.",
        }
    }


PIPE_REASON = (
    "Shell pipe (`|`) defeats Peter's Bash allowlist (matcher expects "
    "the call to start with `git`/`rg`/`cargo`/etc., not a compound). "
    "Use the tool's native cap instead: `rg pattern -m 10` not "
    "`rg ... | head -10`; `git log -n 10` not `git log | head`; "
    "`fd 'foo.*\\.rs'` not `rg --files | grep foo`; `sort -u file` "
    "standalone not `cmd | sort -u`; `wc -l file` not `cmd | wc -l`."
)

CD_REASON = (
    "`cd <dir> && cmd` prefix bypasses the allowlist. cwd is already "
    "the project root. For a different cargo target use "
    "`--manifest-path`; otherwise run a dedicated Bash call without "
    "the `cd &&` chain."
)


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except json.JSONDecodeError:
        # Malformed input — let the call through rather than block
        # everything if the hook plumbing breaks.
        return 0

    cmd = data.get("tool_input", {}).get("command", "")
    if not cmd:
        return 0

    stripped = strip_quoted(cmd)

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
