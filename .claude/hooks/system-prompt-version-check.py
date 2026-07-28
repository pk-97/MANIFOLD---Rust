#!/usr/bin/env python3
"""SessionStart hook: warn when Claude Code has upgraded past the version the
custom system-prompt fork was extracted from.

The fork (.claude/hooks/system-prompt/manifold.md) was cut from the default
prompt of a specific CC version (base-<version>.md). The default co-evolves
with the binary; a silent upgrade means the fork may be missing new harness
contract text. This check makes the drift loud. Fails open."""
import glob
import json
import os
import re
import subprocess
import sys


def main() -> int:
    try:
        base_dir = os.path.join(os.path.dirname(__file__), "system-prompt")
        bases = sorted(glob.glob(os.path.join(base_dir, "base-*.md")))
        if not bases:
            return 0
        recorded = re.search(r"base-([\d.]+)\.md", bases[-1]).group(1).rstrip(".")
        out = subprocess.run(["claude", "--version"], capture_output=True,
                             text=True, timeout=10).stdout
        m = re.search(r"[\d]+\.[\d]+\.[\d]+", out)
        if not m or m.group(0) == recorded:
            return 0
        print(json.dumps({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": (
                    f"system-prompt fork drift: Claude Code is {m.group(0)} but the "
                    f"custom prompt fork was extracted from {recorded}. Before trusting "
                    "sessions launched with --system-prompt-file, re-extract the default "
                    "prompt, diff against .claude/hooks/system-prompt/base-" + recorded +
                    ".md, and fold any new harness-contract text into manifold.md."
                ),
            }
        }))
        return 0
    except Exception:
        return 0


if __name__ == "__main__":
    sys.exit(main())
