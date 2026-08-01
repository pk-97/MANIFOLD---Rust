#!/usr/bin/env python3
"""Mechanical landing ceremony (GIT_TREE_DISCIPLINE.md section 2 (Landing protocol)).

One command after the lead's review passes: merges origin/main into the
branch (in its slot worktree), runs landing_gate.py, merges --no-ff to main
in the main checkout, pushes, optionally closes beads, deletes the branch
when it is an ancestor of origin/main.

The JUDGMENT stays with the lead: the review, the named-red call (pass
--named-red BUG-id --reason "..."), the design-doc status edits. This
script is the fixed git+gate sequence only — every step exits on failure
with the step named, and push happens only after a green gate (or an
explicit named red).

Usage:
  scripts/land_branch.py <branch> --worktree <path> --message '<merge msg>' \
      [--named-red BUG-xxxx --reason '<why safe>'] \
      [--close-bead BUG-xxxx ...] [--close-reason '<closing note>'] \
      [--lead 'k3 (lead)']

Obsolete when: the landing protocol itself changes shape (edit both).
"""

import argparse
import subprocess
import sys
from pathlib import Path

MAIN = Path("/Users/peterkiemann/MANIFOLD - Rust")


def step(name, cmd, cwd, check=True):
    print(f"[land] {name}: {' '.join(cmd)}", flush=True)
    r = subprocess.run(cmd, cwd=str(cwd), capture_output=True, text=True)
    if r.returncode != 0 and check:
        print(f"[land] FAILED at {name}:\n{r.stdout}\n{r.stderr}", file=sys.stderr)
        sys.exit(1)
    return r


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("branch")
    p.add_argument("--worktree", required=True)
    p.add_argument("--message", required=True)
    p.add_argument("--named-red")
    p.add_argument("--reason")
    p.add_argument("--close-bead", action="append", default=[])
    p.add_argument("--close-reason", default="")
    p.add_argument("--lead", default="k3 (lead)")
    a = p.parse_args()

    wt = Path(a.worktree)
    assert wt.exists(), f"worktree {wt} missing"
    assert a.branch != "main", "land a branch, never main itself"

    step("fetch", ["git", "fetch", "origin", "main"], MAIN)
    step("merge origin/main into branch", ["git", "merge", "origin/main", "--no-edit"], wt)

    gate = step("landing_gate", ["scripts/landing_gate.py"], wt, check=False)
    gate_out = gate.stdout + gate.stderr
    print(gate_out[-2000:], flush=True)
    if gate.returncode != 0:
        if not (a.named_red and a.reason):
            print("[land] gate red and no --named-red/--reason given — stopping. "
                  "Review the failure; land over it only with an explicit named red.", file=sys.stderr)
            sys.exit(1)
        step("no-gate verdict", ["scripts/gate_runner.py", "no-gate", "--task", a.named_red,
                                 "--reason", f"{a.reason} {a.lead}"], MAIN)
        step("commit verdict", ["git", "add", "--", ".beads/interactions.jsonl"], MAIN, check=False)
        step("commit verdict", ["git", "commit", "-m",
                                f"beads: no-gate verdict on {a.named_red} for landing {a.branch}. {a.lead}",
                                "--", ".beads/interactions.jsonl"], MAIN, check=False)

    step("merge --no-ff to main", ["git", "merge", "--no-ff", a.branch, "-m", a.message], MAIN)
    step("push main", ["git", "push", "origin", "main"], MAIN)

    for bead in a.close_bead:
        step(f"close {bead}", ["bd", "close", bead, "-r", f"{a.close_reason} {a.lead}"], MAIN)
    if a.close_bead:
        step("commit beads", ["git", "add", "--", ".beads/issues.jsonl"], MAIN)
        step("commit beads", ["git", "commit", "-m",
                              f"beads: {', '.join(a.close_bead)} closed with the {a.branch} landing. {a.lead}",
                              "--", ".beads/issues.jsonl"], MAIN)
        step("push beads", ["git", "push", "origin", "main"], MAIN)

    anc = subprocess.run(["git", "merge-base", "--is-ancestor", a.branch, "origin/main"],
                         cwd=str(MAIN)).returncode == 0
    if anc:
        r = step("delete branch", ["git", "branch", "-d", a.branch], MAIN, check=False)
        if r.returncode != 0:
            # The common cause: the acquiring worktree still has the branch
            # checked out (git refuses). A silent survivals means the next
            # session commits onto a "landed" branch and needs a second
            # landing (self-observed 2026-08-01).
            print(f"[land] NOTE: branch delete failed ({r.stderr.strip()[:200]}) — "
                  f"{a.branch} is fully landed; delete it after its worktree moves off.")
    else:
        print(f"[land] NOTE: {a.branch} tip is not an ancestor of origin/main — left undeleted.")

    print(f"[land] DONE: {a.branch} landed. {a.lead}", flush=True)


if __name__ == "__main__":
    main()
