#!/usr/bin/env python3
"""Freshness test: every path glob in the nudge table matches at least one
tracked file, and every snippet exists. Instruction sets rot like memory did;
this is how we notice. Run: python3 test_context_nudge_globs.py"""
import fnmatch
import json
import os
import subprocess
import sys

BASE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "context-nudges")
REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

table = json.load(open(os.path.join(BASE, "table.json")))["nudges"]
files = subprocess.run(["git", "ls-files"], cwd=REPO, capture_output=True,
                       text=True).stdout.splitlines()

failures = []
for entry in table:
    snippet = os.path.join(BASE, entry["snippet"])
    if not os.path.exists(snippet):
        failures.append(f"{entry['topic']}: snippet missing ({entry['snippet']})")
    for g in entry.get("globs", []):
        # hook matches absolute paths; ls-files is repo-relative — test with a slash prefix
        if not any(fnmatch.fnmatch("/" + f, g) for f in files):
            failures.append(f"{entry['topic']}: glob matches no tracked file ({g})")
    # command_globs are free-form command patterns; only check they're non-empty
    for g in entry.get("command_globs", []):
        if not g.strip("*").strip():
            failures.append(f"{entry['topic']}: vacuous command glob ({g})")

for f in failures:
    print("FAIL:", f)
print(f"{len(table) - len({f.split(':')[0] for f in failures})}/{len(table)} topics clean")
sys.exit(1 if failures else 0)
