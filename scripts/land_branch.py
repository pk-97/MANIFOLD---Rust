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
explicit named red over a gate that ran every check). --named-red requests
the complete gate run; ordinary landings stop before expensive legs on cheap reds.

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
from datetime import datetime, timezone
import time
from pathlib import Path

from landing_gate import (CHECKS_RED, Cancelled, cancellation_signals, record_incomplete,
                          run_cmd, landing_log_path, stop_child)

MAIN = Path("/Users/peterkiemann/MANIFOLD - Rust")


def step(name, cmd, cwd, check=True):
    print(f"[land] {name}: {' '.join(cmd)}", flush=True)
    live = landing_log_path(cwd, 'land-' + name.replace(' ', '-').replace('/', '-'))
    print(f"[RUN] land/{name} (live transcript: {live})", flush=True)
    code, out, err, _ = run_cmd(cmd, cwd, timeout=3600, live_log=live)
    r = subprocess.CompletedProcess(cmd, code, out, err)
    print(f"[{'PASS' if code == 0 else 'FAIL'}] land/{name}", flush=True)
    if r.returncode != 0 and check:
        print(f"[land] FAILED at {name}:\n{r.stdout}\n{r.stderr}", file=sys.stderr)
        sys.exit(1)
    return r


def run_landing_gate(cmd, cwd, log_path):
    """Expose check progress immediately and preserve the complete gate log."""
    print(f"[land] landing_gate: {' '.join(cmd)}", flush=True)
    print(f"[land] complete landing gate transcript: {log_path}", flush=True)
    with log_path.open("w") as log:
        proc = subprocess.Popen(
            cmd, cwd=str(cwd), stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, bufsize=1, start_new_session=True)
        try:
            for line in proc.stdout:
                log.write(line)
                log.flush()
                print(line, end="", flush=True)
            return proc.wait()
        except BaseException:
            stop_child(proc, graceful=True)
            log.write("[INCOMPLETE] landing gate: parent cancelled; children stopped\n")
            log.flush()
            raise
        finally:
            proc.stdout.close()


def merge_gated_tree(branch, worktree, message, gate_cmd, gate_log, gated_commit):
    """Commit only the gated tree, including when main moves during the gate.

    --no-commit leaves the proposed merge inspectable before it becomes trunk.
    A changed merge tree is aborted, merged into the slot, and gated again.
    Content-addressed leg passes make that retry proportional to the change.
    """
    while True:
        gated = step('gated tree', ['git', 'rev-parse', f'{gated_commit}^{{tree}}'], worktree).stdout.strip()
        merged = step('merge --no-ff to main',
                      ['git', 'merge', '--no-ff', '--no-commit', gated_commit, '-m', message], MAIN,
                      check=False)
        if merged.returncode:
            step('abort failed landing merge', ['git', 'merge', '--abort'], MAIN, check=False)
            print('[land] main merge conflicted; resolve current main in the slot and retry.', file=sys.stderr)
            sys.exit(1)
        tree = step('verify merge tree', ['git', 'write-tree'], MAIN).stdout.strip()
        if tree == gated:
            pending = step('pending merge', ['git', 'rev-parse', '-q', '--verify', 'MERGE_HEAD'],
                           MAIN, check=False)
            if pending.returncode == 0:
                step('commit gated merge', ['git', 'commit', '--no-edit'], MAIN)
            return
        step('abort changed merge tree', ['git', 'merge', '--abort'], MAIN)
        step('merge moved main into branch', ['git', 'merge', 'main', '--no-edit'], worktree)
        gated_commit = step('pin gate commit', ['git', 'rev-parse', 'HEAD'], worktree).stdout.strip()
        # Even a named-red override was for the earlier tree. A changed tree
        # must earn green or return to the lead for another named-red review.
        if run_landing_gate(gate_cmd, worktree, gate_log):
            print('[land] moved-main gate red; stopping before main changes.', file=sys.stderr)
            sys.exit(1)


def main():
    try:
        with cancellation_signals():
            return _main()
    except Cancelled as error:
        repo = Path(sys.argv[sys.argv.index('--worktree') + 1]) if '--worktree' in sys.argv else Path.cwd()
        return record_incomplete(repo, error)


def _main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("branch")
    p.add_argument("--worktree", required=True)
    p.add_argument("--message", required=True)
    p.add_argument("--named-red")
    p.add_argument("--reason")
    p.add_argument("--skip-gpu", metavar="REASON",
                   help="defer GPU proofs with a recorded reason; all other gates must pass")
    p.add_argument("--close-bead", action="append", default=[])
    p.add_argument("--close-reason", default="")
    p.add_argument("--lead", default="k3 (lead)")
    a = p.parse_args()

    wt = Path(a.worktree)
    assert wt.exists(), f"worktree {wt} missing"
    assert a.branch != "main", "land a branch, never main itself"

    step("fetch", ["git", "fetch", "origin", "main"], MAIN)
    step("merge origin/main into branch", ["git", "merge", "origin/main", "--no-edit"], wt)

    # A reviewed named red needs every leg's result, including when the known
    # red is cheap. Completion is still enforced by CHECKS_RED below.
    gate_cmd = [sys.executable, "-u", "scripts/landing_gate.py", "--repo", str(wt.resolve())]
    if a.named_red:
        gate_cmd.append("--keep-going")
    if a.skip_gpu:
        gate_cmd += ["--skip-gpu", a.skip_gpu]
    log_dir = wt / "target" / "landing-logs"
    log_dir.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    gate_log = (log_dir / f"landing-gate-{stamp}-{time.time_ns()}.log").resolve()
    gated_commit = step('pin gate commit', ['git', 'rev-parse', 'HEAD'], wt).stdout.strip()
    gate_returncode = run_landing_gate(gate_cmd, wt, gate_log)
    if gate_returncode != 0:
        if a.skip_gpu or not (a.named_red and a.reason):
            print("[land] gate red and no --named-red/--reason given — stopping. "
                  "Review the failure; land over it only with an explicit named red.", file=sys.stderr)
            sys.exit(1)
        if gate_returncode != CHECKS_RED:
            print(f"[land] the gate exited {gate_returncode} without running every check (a refusal, "
                  "a crash or a moved tree); a named red covers only checks that ran red — stopping.",
                  file=sys.stderr)
            sys.exit(1)
        step("no-gate verdict", ["scripts/gate_runner.py", "no-gate", "--task", a.named_red,
                                 "--reason", f"{a.reason} {a.lead}"], MAIN)

    merge_gated_tree(a.branch, wt, a.message, gate_cmd, gate_log, gated_commit)
    if gate_returncode != 0:
        # After the merge: a verdict commit before it moves main, so the
        # merge tree no longer matches the gated tree and the gate reruns red.
        step("commit verdict", ["git", "add", "--", ".beads/interactions.jsonl"], MAIN, check=False)
        step("commit verdict", ["git", "commit", "-m",
                                f"beads: no-gate verdict on {a.named_red} for landing {a.branch}. {a.lead}",
                                "--", ".beads/interactions.jsonl"], MAIN, check=False)
    step("push main", ["git", "push", "origin", "main"], MAIN)

    for bead in a.close_bead:
        step(f"close {bead}", ["bd", "close", bead, "-r", f"{a.close_reason} {a.lead}"], MAIN)
    if a.close_bead:
        step("commit beads", ["git", "add", "--", ".beads/issues.jsonl"], MAIN)
        step("commit beads", ["git", "commit", "-m",
                              f"beads: {', '.join(a.close_bead)} closed with the {a.branch} landing. {a.lead}",
                              "--", ".beads/issues.jsonl"], MAIN)
        step("push beads", ["git", "push", "origin", "main"], MAIN)

    anc = step("verify landed ancestry", ["git", "merge-base", "--is-ancestor", a.branch, "origin/main"],
               MAIN, check=False).returncode == 0
    if anc:
        if wt.resolve().parent == (MAIN / ".claude/worktrees").resolve():
            step("release landed slot", [sys.executable, str(MAIN / "scripts/agent-worktree.py"),
                                         "release", wt.name], MAIN)
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
    sys.exit(main())
