#!/usr/bin/env python3
"""PostToolUse guard: no test run may pass without a visible summary line.

Failure mode this closes (2026-07-17, depth-relight orchestration): agents
launch `cargo test` / `cargo nextest` — foreground with a truncating pipe
(`| tail -1`), or as a background task — and then either idle-wait on a
process that already exited or report "green" from an exit code without ever
reading a `test result:` line. Silent test failures and stalled agents both
trace back to the same sin: the summary line was never in anyone's context.

Mechanism: fires on every Bash PostToolUse. If the command is a test
invocation, require the canonical summary evidence in the captured output —
`test result:` (cargo test, per-binary) or `Summary [` (nextest). Missing
evidence injects a loud warning into the calling agent's own context
(PostToolUse additionalContext reaches subagents in their own context —
probe-verified 2026-07-04, see daemon-posttooluse.py). Background launches
get the warning up front, at launch time, since their output arrives later.

This is advisory context, not a block — the agent may be mid-pipeline — but
it is injected EVERY time, so "forgot to check" can no longer survive a
turn. Fails open on any error.
"""
import json
import re
import shlex
import sys

# Evidence that a test run's outcome was actually surfaced.
SUMMARY = re.compile(r"test result:|Summary \[")

# Shell operators that separate command positions (mirror of
# preToolUseBash.py's segment split — kept local so this guard has no
# import-order coupling; the set only needs to be conservative).
_OPS = {"&&", "||", ";", "|", "&"}


def _is_test_invocation(command: str) -> bool:
    """True iff some command POSITION is a real `cargo test`/`cargo nextest`
    run. Tokenizes with shlex (same approach as preToolUseBash.py's
    `_shlex_segments`) so quoted strings — commit messages, python -c
    scripts, heredoc bodies — can never false-positive (three self-observed
    false positives drove this; regex-over-raw-text is unfixable here).
    Heredoc bodies are excluded by matching only the first line. Malformed
    quoting = not a match (fail-open toward silence)."""
    first_line = command.split("\n", 1)[0]
    try:
        tokens = shlex.split(first_line, posix=True)
    except ValueError:
        return False
    segments, current = [], []
    for t in tokens:
        if t in _OPS:
            if current:
                segments.append(current)
            current = []
        else:
            current.append(t)
    if current:
        segments.append(current)
    for seg in segments:
        # find `cargo` allowing env-var prefixes (FOO=bar cargo test ...)
        idx = next((i for i, t in enumerate(seg) if "=" not in t), None)
        if idx is None or not seg[idx].endswith("cargo"):
            continue
        rest = seg[idx + 1 :]
        sub = next((t for t in rest if not t.startswith("-")), "")
        if sub == "nextest" or (sub == "test" and "--help" not in rest and "--list" not in rest):
            return True
    return False


def main() -> None:
    try:
        data = json.load(sys.stdin)
        if data.get("tool_name") != "Bash":
            return
        tool_input = data.get("tool_input") or {}
        command = tool_input.get("command", "")
        if not _is_test_invocation(command):
            return

        if tool_input.get("run_in_background"):
            warn = (
                "BACKGROUND TEST RUN LAUNCHED — harness contract: when its "
                "task notification arrives you MUST read the output file and "
                "quote the `test result:` / `Summary [` line(s) before "
                "treating the run as green or continuing dependent work. An "
                "exited process with no summary line is a silently failed "
                "run: rerun it in the foreground. Never idle-wait on a test "
                "process without verifying it is still alive (`ps`)."
            )
        else:
            output = data.get("tool_response") or {}
            if isinstance(output, dict):
                text = str(output.get("stdout", "")) + str(output.get("stderr", ""))
                if not text.strip():
                    text = json.dumps(output)
            else:
                text = str(output)
            if SUMMARY.search(text):
                return
            warn = (
                "UNVERIFIED TEST RUN — this command executed tests but its "
                "visible output contains no `test result:` / `Summary [` "
                "line (truncated pipe, or the run died before summarizing). "
                "Do NOT report or rely on this run as green. Re-run showing "
                "the summary (e.g. `... 2>&1 | rg 'test result:|Summary'`) "
                "or rerun without the truncating filter, and quote the "
                "line(s) in your report."
            )

        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "PostToolUse",
                        "additionalContext": warn,
                    }
                }
            )
        )
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
