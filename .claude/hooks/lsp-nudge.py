#!/usr/bin/env python3
"""PreToolUse(Bash) nudge: redirect symbol-shaped rg/grep to the LSP tool.

Why this exists: Claude Code's system prompt biases the model toward grep, and a
passive "prefer LSP" line in CLAUDE.md loses to it (~1% real-world LSP use). The
reliable redirect is a *narrow soft-block*: when a search is clearly a Rust
symbol/definition/impl query, deny it with a reason that names the LSP operation
to use instead. The model sees the deny reason and adapts.

Deliberately narrow to keep false positives near zero — it fires ONLY on
definition-shaped patterns (`fn name`, `trait name`, `struct name`, `enum name`)
and trait-impl shapes (`impl ... for`), and ONLY when the search sweeps a
workspace or directory. A grep aimed at an explicit file path is reading
intent — the agent already knows where the code lives and wants the lines;
LSP's rationale (false hits across crates, trait dispatch) doesn't apply, and
denying it just costs a round trip (2026-07-05: transcript audit showed this
false-positive class training agents to blanket-append `#grep-ok`, which makes
the hook inert for the whole session). A bare keyword (`rg "trait"`), a plain
identifier, a string/JSON/log/doc search — all pass untouched.

Bypass: append `#grep-ok` to the command to force the text search through. The
cost of a false positive is therefore one of: re-issue as an LSP call (the point),
or re-run with `#grep-ok`.

`#grep-ok` MUST be the last thing on its physical line. `#` starts a real shell
comment, so anything chained after the marker on the same line (`#grep-ok; cmd`,
`#grep-ok && cmd`) is silently swallowed by the shell and never runs — this bit
a session on 2026-07-06 (a required daemon self-grade append was dropped this
way; see `.claude/daemon/eval/observations.session.jsonl`). This hook now denies
that shape outright instead of letting the model discover it by lost output.

Runs as a second PreToolUse Bash hook alongside preToolUseBash.py. A `deny` here
overrides that hook's `allow` (deny takes precedence across hooks).

Receives {"tool_name": "Bash", "tool_input": {"command": "..."}} on stdin.
Emits hookSpecificOutput.permissionDecision="deny" + reason, or nothing.
"""
import json
import re
import shlex
import sys

# Extensions that mark a token as an explicit single-file target. Anything the
# def/impl patterns could plausibly be grepped for in this repo.
_FILE_EXT = re.compile(r"\.(rs|toml|md|json|wgsl|metal|py|txt|yaml|yml)$")

_GREP_OK = "#grep-ok"


def _unquoted_mask(cmd: str):
    """Per-character mask, True where cmd[i] sits inside a shell quote (or is
    an escaped char), False where it's real unquoted shell text.

    Just enough shell-quoting awareness to tell a genuine `#grep-ok` marker
    apart from the same literal text appearing as quoted data (e.g. inside an
    `rg '...'` search pattern or a `printf` payload).
    """
    quoted = [False] * len(cmd)
    state = None  # None | "'" | '"'
    escaped = False
    for i, ch in enumerate(cmd):
        if state == "'":
            quoted[i] = True
            if ch == "'":
                state = None
            continue
        if escaped:
            quoted[i] = True
            escaped = False
            continue
        if ch == "\\" and state != "'":
            quoted[i] = True
            escaped = True
            continue
        if state == '"':
            quoted[i] = True
            if ch == '"':
                state = None
            continue
        if ch == "'":
            quoted[i] = True
            state = "'"
            continue
        if ch == '"':
            quoted[i] = True
            state = '"'
            continue
        quoted[i] = False
    return quoted


def _grep_ok_status(cmd: str) -> str:
    """Classify every unquoted `#grep-ok` occurrence in `cmd`.

    Returns "absent" (no real marker anywhere), "valid" (every real marker
    runs to the end of its physical line, i.e. is an actual shell comment with
    nothing after it), or "trailing" (some real marker has more command text
    after it on the same line — that text is inside the shell comment and
    will NOT execute).
    """
    quoted = _unquoted_mask(cmd)
    found_any = False
    for m in re.finditer(re.escape(_GREP_OK), cmd):
        start, end = m.start(), m.end()
        if any(quoted[start:end]):
            continue  # `#grep-ok` here is quoted data, not a real marker
        found_any = True
        newline = cmd.find("\n", end)
        tail = cmd[end:] if newline == -1 else cmd[end:newline]
        if tail.strip():
            return "trailing"
    return "valid" if found_any else "absent"


def _targets_explicit_file(cmd: str) -> bool:
    """True if any argument is a concrete file path (not a glob, not a dir).

    A search aimed at a named file is reading intent, not symbol lookup —
    the false-hit / trait-dispatch rationale for LSP doesn't apply there.
    """
    try:
        tokens = shlex.split(cmd)
    except ValueError:
        # Unbalanced quotes etc. — fall back to a conservative scan of the raw
        # string for a path-ish run ending in a known extension.
        tokens = cmd.split()
    return any("*" not in t and "?" not in t and _FILE_EXT.search(t) for t in tokens)


def decide(cmd: str):
    """Return a deny reason string, or None to let the command through."""
    if not cmd:
        return None

    grep_ok = _grep_ok_status(cmd)
    if grep_ok == "trailing":
        return (
            "`#grep-ok` has more command text after it on the same line. `#` starts a "
            "real shell comment, so everything after `#grep-ok` on that line is silently "
            "DISCARDED and will not run — this has already dropped a required command once. "
            "Put `#grep-ok` at the very end of the line, or split the trailing command into "
            "a separate Bash call."
        )

    # Explicit bypass — the model meant a text search.
    if grep_ok == "valid":
        return None

    # Only consider commands that actually run a text searcher.
    if not re.search(r"\b(rg|grep|egrep|fgrep|ack|ag)\b", cmd):
        return None

    # Trait-impl shape -> goToImplementation.
    impl_shape = re.search(r"\bimpl\b.*\bfor\b", cmd) or re.search(r"\bimpl\s+[A-Za-z_]", cmd)
    # Definition shape -> workspaceSymbol / goToDefinition.
    def_shape = re.search(r"\b(fn|trait|struct|enum)\s+[A-Za-z_]", cmd)

    if not (impl_shape or def_shape):
        return None

    # Single-file exemption: an explicit file argument means the agent already
    # located the code and wants to read it. Globs (-g '*.rs') and directory
    # sweeps still fire.
    if _targets_explicit_file(cmd):
        return None

    if impl_shape:
        op = "goToImplementation on the trait (lists every implementor across crates) — or findReferences"
    else:
        op = "workspaceSymbol by name, or goToDefinition on a use site (documentSymbol for a single file)"

    return (
        "Symbol-shaped search detected. The LSP tool (rust-analyzer) is more reliable here "
        "than text grep: no false hits from comments/strings/same-named methods, and it "
        "follows trait dispatch and re-exports across crates.\n"
        f"Use the LSP tool: {op}.\n"
        "If you genuinely want a TEXT search (strings, JSON, logs, comments, a .rs literal), "
        "re-run the SAME command with `#grep-ok` appended to bypass this."
    )


def main():
    try:
        data = json.load(sys.stdin)
    except Exception:
        sys.exit(0)

    cmd = ((data.get("tool_input") or {}).get("command") or "")
    reason = decide(cmd)
    if reason is None:
        sys.exit(0)

    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }))
    sys.exit(0)


if __name__ == "__main__":
    main()
