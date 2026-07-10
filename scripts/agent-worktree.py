#!/usr/bin/env python3
"""Worktree pool for agent execution.

Reuses an idle WARM worktree (checkout -B) instead of minting a cold one —
a fresh worktree pays a full cold cargo build (observed: 5-41 GB targets,
~10 min). Spec: .claude/GIT_TREE_DISCIPLINE.md §2c.

Usage:
  python3 scripts/agent-worktree.py list
  python3 scripts/agent-worktree.py acquire <name> <branch> [--tip REF] [--owner TEXT]
  python3 scripts/agent-worktree.py release <name>

`acquire` prints the reused-or-created worktree path plus the step-0
base-verification line (`git log --oneline -1`). The CALLER must confirm that
line matches the intended tip before doing any work — the script verifies
mechanics, not intent.

A worktree is idle (reusable) when ALL hold:
  - `git status --porcelain` is empty (WORKTREE_HANDOFF.md counts as dirt —
    a stopped session's unfinished work is a busy signal, see §3b);
  - its HEAD is an ancestor of origin/main (the work landed);
  - its lease file is absent or older than LEASE_TTL_HOURS.
"""

import argparse
import json
import shutil
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
POOL = REPO / ".claude" / "worktrees"
LEASE_NAME = ".worktree-lease.json"  # gitignored; mtime is the staleness clock
LEASE_TTL_HOURS = 8


def git(cwd, *args, check=True):
    result = subprocess.run(
        ["git", "-C", str(cwd), *args], capture_output=True, text=True
    )
    if check and result.returncode != 0:
        sys.exit(f"git -C {cwd} {' '.join(args)} failed:\n{result.stderr.strip()}")
    return result


def is_landed(wt):
    head = git(wt, "rev-parse", "HEAD").stdout.strip()
    return git(REPO, "merge-base", "--is-ancestor", head, "origin/main",
               check=False).returncode == 0


def idle_state(wt):
    """Returns (idle: bool, reason: str)."""
    dirt = git(wt, "status", "--porcelain").stdout.strip()
    if dirt:
        return False, f"dirty ({len(dirt.splitlines())} paths)"
    if not is_landed(wt):
        return False, "branch not landed on origin/main"
    lease = wt / LEASE_NAME
    if lease.exists():
        age_h = (time.time() - lease.stat().st_mtime) / 3600
        if age_h < LEASE_TTL_HOURS:
            try:
                owner = json.loads(lease.read_text()).get("owner", "?")
            except (json.JSONDecodeError, OSError):
                owner = "?"
            return False, f"leased by {owner} ({age_h:.1f}h ago)"
    return True, "idle"


def pool_worktrees():
    if not POOL.is_dir():
        return []
    return sorted(p for p in POOL.iterdir() if (p / ".git").exists())


def target_bytes(wt):
    t = wt / "target"
    if not t.is_dir():
        return 0
    # du -sk is far faster than a python walk over a multi-GB tree.
    out = subprocess.run(["du", "-sk", str(t)], capture_output=True, text=True)
    return int(out.stdout.split()[0]) * 1024 if out.returncode == 0 else 0


def copy_missing_fixtures(wt):
    """Copy files under any tests/fixtures dir that the checkout didn't bring
    (gitignored .manifold projects, downloaded assets). Only adds; never
    overwrites."""
    copied = 0
    for src_dir in REPO.rglob("tests/fixtures"):
        rel_parts = src_dir.relative_to(REPO).parts
        if rel_parts[:2] == (".claude", "worktrees") or "target" in rel_parts:
            continue
        for src in src_dir.rglob("*"):
            if not src.is_file():
                continue
            dst = wt / src.relative_to(REPO)
            if not dst.exists():
                dst.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(src, dst)
                copied += 1
    return copied


def verify_and_report(wt):
    head_line = git(wt, "log", "--oneline", "-1").stdout.strip()
    branch = git(wt, "branch", "--show-current").stdout.strip()
    print(f"WORKTREE: {wt}")
    print(f"BRANCH:   {branch}")
    print(f"HEAD:     {head_line}")
    print("VERIFY:   confirm HEAD matches your intended tip (step-0 guard) "
          "before any work.")


def cmd_list(_args):
    for wt in pool_worktrees():
        idle, reason = idle_state(wt)
        branch = git(wt, "branch", "--show-current").stdout.strip() or "(detached)"
        head = git(wt, "rev-parse", "--short", "HEAD").stdout.strip()
        warm = f"{target_bytes(wt) / 2**30:.1f}G target" if target_bytes(wt) else "cold"
        print(f"{'IDLE' if idle else 'BUSY':4}  {wt.name:28} {branch:40} "
              f"{head}  {warm:14} {reason}")


def cmd_acquire(args):
    git(REPO, "fetch", "origin", "main")
    tip = args.tip or "origin/main"
    # Prefer the warmest idle worktree; fall back to creating a cold one.
    idle = [wt for wt in pool_worktrees() if idle_state(wt)[0]]
    if idle:
        wt = max(idle, key=target_bytes)
        git(wt, "checkout", "-B", args.branch, tip)
        handoff = wt / "WORKTREE_HANDOFF.md"
        if handoff.exists():
            handoff.unlink()  # unreachable while dirty-checked, kept for safety
        print(f"REUSED idle worktree ({target_bytes(wt) / 2**30:.1f}G warm target)")
    else:
        wt = POOL / args.name
        git(REPO, "worktree", "add", "-b", args.branch, str(wt), tip)
        print("CREATED fresh worktree (pool had no idle entry — cold build ahead)")
    (wt / LEASE_NAME).write_text(json.dumps(
        {"owner": args.owner, "branch": args.branch,
         "acquired": time.strftime("%Y-%m-%dT%H:%M:%S%z")}) + "\n")
    copied = copy_missing_fixtures(wt)
    print(f"FIXTURES: {copied} file(s) copied from main checkout")
    verify_and_report(wt)


def cmd_release(args):
    wt = POOL / args.name
    lease = wt / LEASE_NAME
    if lease.exists():
        lease.unlink()
        print(f"released {wt}")
    else:
        print(f"no lease on {wt} — nothing to do")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("list")
    acq = sub.add_parser("acquire")
    acq.add_argument("name")
    acq.add_argument("branch")
    acq.add_argument("--tip", default=None,
                     help="base commit/ref (default: origin/main after fetch)")
    acq.add_argument("--owner", default="unnamed-session",
                     help="who holds the lease (session id or label)")
    rel = sub.add_parser("release")
    rel.add_argument("name")
    args = parser.parse_args()
    {"list": cmd_list, "acquire": cmd_acquire, "release": cmd_release}[args.cmd](args)


if __name__ == "__main__":
    main()
