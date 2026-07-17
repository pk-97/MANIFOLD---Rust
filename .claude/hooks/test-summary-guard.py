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
import sys

TEST_CMD = re.compile(r"\bcargo\b[^|;&]*\b(nextest|test)\b")
# Evidence that a test run's outcome was actually surfaced.
SUMMARY = re.compile(r"test result:|Summary \[")
# Commands that mention cargo test but don't run tests.
NON_RUN = re.compile(r"--help|--list\b|\bcargo\s+(clippy|build|check|run)\b")


def main() -> None:
    try:
        data = json.load(sys.stdin)
        if data.get("tool_name") != "Bash":
            return
        tool_input = data.get("tool_input") or {}
        command = tool_input.get("command", "")
        if not TEST_CMD.search(command) or NON_RUN.search(command):
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
