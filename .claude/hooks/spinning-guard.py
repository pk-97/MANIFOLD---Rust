#!/usr/bin/env python3
"""PostToolUse(Edit|Write|Bash) — spinning detector.

Transcript analysis (2026-07-27) found the expensive failure mode is editing
the same file or re-running near-identical commands many times with nothing
landed in between (worst cases: 28 edits, 18 identical probes, zero commits).
Healthy iteration has commits or beads interleaved; spinning does not.

Counts per-session edits per file and repeats of normalized Bash commands.
Any `git commit` or `bd create/close` resets all counters (an artifact
landed). Warns at the thresholds below — advisory only, never blocks,
because Peter-driven feedback loops legitimately edit one file many times.
Fails open on any error.
"""
import json
import re
import sys

FILE_WARN = (8, 12)   # same file edited N times with nothing landed
CMD_WARN = (6, 10)    # near-identical command run N times with nothing landed


def normalize_cmd(cmd: str) -> str:
    cmd = re.sub(r"\s+", " ", cmd.strip())
    return re.sub(r"\d+", "N", cmd)[:200]


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except Exception:
        return 0

    tool = data.get("tool_name")
    if tool not in ("Edit", "Write", "MultiEdit", "Bash"):
        return 0

    session = data.get("session_id") or "nosession"
    state_path = f"/tmp/spinning_guard_{session}.json"
    try:
        state = json.load(open(state_path))
    except Exception:
        state = {"files": {}, "cmds": {}}

    warn = None
    tin = data.get("tool_input") or {}

    if tool == "Bash":
        cmd = tin.get("command") or ""
        if "git commit" in cmd or "bd create" in cmd or "bd close" in cmd:
            state = {"files": {}, "cmds": {}}  # artifact landed — not spinning
        else:
            key = normalize_cmd(cmd)
            n = state["cmds"].get(key, 0) + 1
            state["cmds"][key] = n
            if n in CMD_WARN:
                warn = (
                    f"spinning-guard: this is run #{n} of a near-identical command with "
                    "nothing committed or logged in between. If the output isn't teaching "
                    "you something new, stop probing — step up the debug ladder "
                    "(seam review, then the read-only consult) instead of running it again."
                )
    else:
        path = tin.get("file_path") or ""
        if path:
            n = state["files"].get(path, 0) + 1
            state["files"][path] = n
            if n in FILE_WARN:
                warn = (
                    f"spinning-guard: edit #{n} to {path.rsplit('/', 1)[-1]} with nothing "
                    "committed or logged since. If you're iterating against feedback that's "
                    "fine; if you're guessing, the approach is probably wrong — state the "
                    "root cause before the next edit, or take it up the debug ladder."
                )

    try:
        json.dump(state, open(state_path, "w"))
    except Exception:
        pass

    if warn:
        print(json.dumps({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": warn,
            }
        }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
