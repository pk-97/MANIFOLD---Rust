#!/usr/bin/env python3
"""Worktree ring for agent execution — fixed slot pool, structurally capped.

The pool is a ring of at most MAX_SLOTS worktrees named slot-0..slot-N.
`acquire` reuses the warmest idle slot (checkout -B keeps its cargo target
warm); it creates a new slot only while the ring is below capacity, and
NEVER beyond it — with all slots genuinely busy it exits loudly instead.
Storage blowout is therefore impossible by construction: no code path in
this script (the only sanctioned way to get a worktree; the Bash hook
denies raw `git worktree add`) can grow the pool past MAX_SLOTS.

History: 2026-07-15, 19 per-task worktrees × 15-60 GB targets = 455 GB.
Root cause: the fixture copier used to copy untracked-but-not-ignored
files, so every worktree read as permanently dirty, reuse never fired,
and each acquire minted a fresh dir. Fixtures are now copied only if
gitignored (they never dirty `git status`), and the cap bounds whatever
bug comes next.

Usage:
  python3 scripts/agent-worktree.py list
  python3 scripts/agent-worktree.py acquire <task-label> <branch> [--tip REF] [--owner TEXT]
  python3 scripts/agent-worktree.py release <slot>

`acquire` prints the slot path plus the step-0 base-verification line
(`git log --oneline -1`). The CALLER must confirm that line matches the
intended tip before doing any work — the script verifies mechanics, not
intent. <task-label> is recorded in the lease for `list`; it does NOT
name the directory (slots are anonymous — that anonymity is the fix:
per-task names are what let the old pool grow one dir per task).

A slot is idle (reusable) when ALL hold:
  - `git status --porcelain` is empty (WORKTREE_HANDOFF.md counts as dirt —
    a stopped session's unfinished work is a busy signal, see
    GIT_TREE_DISCIPLINE.md §3b);
  - its HEAD is an ancestor of origin/main (the work landed);
  - its lease file is absent or older than LEASE_TTL_HOURS.

On acquire, a slot whose target/ exceeds TARGET_CAP_GB is wiped before
handoff (stale artifacts of dead branches otherwise accumulate without
bound) — an occasional cold build in exchange for a hard per-slot disk
ceiling. Worst-case pool size: MAX_SLOTS × TARGET_CAP_GB plus checkouts,
roughly 270 GB (cap raised 6→10 on 2026-07-17, Peter's call — slots are
created on demand, so the pool only reaches this if 10 concurrent
workstreams actually happen).

Release is an optimization, not a safety mechanism: a forgotten lease
expires after LEASE_TTL_HOURS and only ever pins one slot.
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
MAX_SLOTS = 10         # hard structural cap — there is no override flag
TARGET_CAP_GB = 25     # per-slot target/ ceiling, enforced at acquire
SLOT_PREFIX = "slot-"


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


def lease_info(wt):
    """Returns (age_hours or None, owner, task) — None age means no lease."""
    lease = wt / LEASE_NAME
    if not lease.exists():
        return None, "", ""
    age_h = (time.time() - lease.stat().st_mtime) / 3600
    try:
        data = json.loads(lease.read_text())
    except (json.JSONDecodeError, OSError):
        data = {}
    return age_h, data.get("owner", "?"), data.get("task", "?")


def idle_state(wt):
    """Returns (idle: bool, reason: str)."""
    dirt = git(wt, "status", "--porcelain").stdout.strip()
    if dirt:
        return False, f"dirty ({len(dirt.splitlines())} paths)"
    if not is_landed(wt):
        return False, "branch not landed on origin/main"
    age_h, owner, task = lease_info(wt)
    if age_h is not None and age_h < LEASE_TTL_HOURS:
        return False, f"leased by {owner} for {task} ({age_h:.1f}h ago)"
    return True, "idle"


def pool_slots():
    if not POOL.is_dir():
        return []
    return sorted(p for p in POOL.iterdir()
                  if p.name.startswith(SLOT_PREFIX) and (p / ".git").exists())


def target_bytes(wt):
    t = wt / "target"
    if not t.is_dir():
        return 0
    # du -sk is far faster than a python walk over a multi-GB tree.
    out = subprocess.run(["du", "-sk", str(t)], capture_output=True, text=True)
    return int(out.stdout.split()[0]) * 1024 if out.returncode == 0 else 0


def enforce_target_cap(wt):
    size = target_bytes(wt)
    if size > TARGET_CAP_GB * 2**30:
        shutil.rmtree(wt / "target", ignore_errors=True)
        print(f"TARGET:   wiped ({size / 2**30:.1f}G exceeded the "
              f"{TARGET_CAP_GB}G per-slot cap — cold build ahead)")


def copy_missing_fixtures(wt):
    """Copy GITIGNORED files under any tests/fixtures dir that the checkout
    didn't bring (.manifold projects, downloaded assets). Ignored files only:
    copying an untracked-but-not-ignored file makes `git status` dirty
    forever, which is exactly the bug that poisoned the old pool. Only adds;
    never overwrites."""
    candidates = []
    for src_dir in REPO.rglob("tests/fixtures"):
        rel_parts = src_dir.relative_to(REPO).parts
        if rel_parts[:2] == (".claude", "worktrees") or "target" in rel_parts:
            continue
        for src in src_dir.rglob("*"):
            if src.is_file() and not (wt / src.relative_to(REPO)).exists():
                candidates.append(src)
    if not candidates:
        return 0
    # Batch-classify: git check-ignore echoes back only the ignored paths.
    rels = [str(p.relative_to(REPO)) for p in candidates]
    out = subprocess.run(
        ["git", "-C", str(REPO), "check-ignore", "--stdin"],
        input="\n".join(rels), capture_output=True, text=True,
    )
    ignored = set(out.stdout.splitlines())
    copied = 0
    for src, rel in zip(candidates, rels):
        if rel not in ignored:
            continue
        dst = wt / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dst)
        copied += 1
    return copied


def verify_and_report(wt):
    head_line = git(wt, "log", "--oneline", "-1").stdout.strip()
    branch = git(wt, "branch", "--show-current").stdout.strip()
    print(f"WORKTREE: {wt}")
    print(f"SLOT:     {wt.name}  (release with: python3 scripts/agent-worktree.py "
          f"release {wt.name})")
    print(f"BRANCH:   {branch}")
    print(f"HEAD:     {head_line}")
    print("VERIFY:   confirm HEAD matches your intended tip (step-0 guard) "
          "before any work.")


def cmd_list(_args):
    slots = pool_slots()
    if not slots:
        print(f"(pool empty — slots are created on demand, cap {MAX_SLOTS})")
        return
    for wt in slots:
        idle, reason = idle_state(wt)
        branch = git(wt, "branch", "--show-current").stdout.strip() or "(detached)"
        head = git(wt, "rev-parse", "--short", "HEAD").stdout.strip()
        warm = f"{target_bytes(wt) / 2**30:.1f}G target" if target_bytes(wt) else "cold"
        print(f"{'IDLE' if idle else 'BUSY':4}  {wt.name:8} {branch:40} "
              f"{head}  {warm:14} {reason}")


def cmd_acquire(args):
    git(REPO, "fetch", "origin", "main")
    tip = args.tip or "origin/main"
    slots = pool_slots()

    idle = [wt for wt in slots if idle_state(wt)[0]]
    if idle:
        wt = max(idle, key=target_bytes)  # warmest target = best build reuse
        enforce_target_cap(wt)
        git(wt, "checkout", "-B", args.branch, tip)
        print(f"REUSED {wt.name} ({target_bytes(wt) / 2**30:.1f}G warm target)")
    elif len(slots) < MAX_SLOTS:
        # Fill the lowest free index so slot names stay dense.
        taken = {wt.name for wt in slots}
        idx = next(i for i in range(MAX_SLOTS)
                   if f"{SLOT_PREFIX}{i}" not in taken)
        wt = POOL / f"{SLOT_PREFIX}{idx}"
        git(REPO, "worktree", "add", "-b", args.branch, str(wt), tip)
        print(f"CREATED {wt.name} (ring at {len(slots) + 1}/{MAX_SLOTS} — "
              "cold build ahead)")
    else:
        for wt in slots:
            _, reason = idle_state(wt)
            print(f"  {wt.name}: {reason}", file=sys.stderr)
        sys.exit(
            f"POOL FULL: all {MAX_SLOTS} slots are busy (states above). The ring "
            "never grows past its cap — this failure is deliberate and loud. "
            "Either release/land a slot's work, wait for a lease to expire "
            f"(TTL {LEASE_TTL_HOURS}h), or surface this to Peter. Do NOT create "
            "a worktree by hand."
        )

    (wt / LEASE_NAME).write_text(json.dumps(
        {"owner": args.owner, "task": args.name, "branch": args.branch,
         "acquired": time.strftime("%Y-%m-%dT%H:%M:%S%z")}) + "\n")
    copied = copy_missing_fixtures(wt)
    print(f"FIXTURES: {copied} gitignored file(s) copied from main checkout")
    verify_and_report(wt)


def cmd_release(args):
    wt = POOL / args.slot
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
    acq.add_argument("name", help="task label, recorded in the lease "
                     "(does NOT name the directory)")
    acq.add_argument("branch")
    acq.add_argument("--tip", default=None,
                     help="base commit/ref (default: origin/main after fetch)")
    acq.add_argument("--owner", default="unnamed-session",
                     help="who holds the lease (session id or label)")
    rel = sub.add_parser("release")
    rel.add_argument("slot", help="slot name printed by acquire (e.g. slot-2)")
    args = parser.parse_args()
    {"list": cmd_list, "acquire": cmd_acquire, "release": cmd_release}[args.cmd](args)


if __name__ == "__main__":
    main()
