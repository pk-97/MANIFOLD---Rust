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
  scripts/agent-worktree.py list
  scripts/agent-worktree.py acquire <task-label> <branch> [--tip REF] [--owner TEXT]
  scripts/agent-worktree.py release <slot>
  scripts/agent-worktree.py scrub

`acquire` prints the slot path plus the step-0 base-verification line
(`git log --oneline -1`). The CALLER must confirm that line matches the
intended tip before doing any work — the script verifies mechanics, not
intent. <task-label> is recorded in the lease for `list`; it does NOT
name the directory (slots are anonymous — that anonymity is the fix:
per-task names are what let the old pool grow one dir per task).

Every slot lands in exactly one category (`slot_state`), and only the first two
are ever handed out automatically:

  IDLE      clean, landed, no lease.
  RECLAIM   finished work the ring can take back by itself: clean AND landed
            with only a stale/dead lease in the way, OR clean and a duplicate
            of a branch another slot already holds (a `checkout -B` artifact —
            the workstream keeps its seat, this copy is spare).
  IN-USE    a live lease or a live session. Wait; never reclaim.
  HUMAN     uncommitted changes, or the SOLE holder of unlanded commits.
            Never automatic, whatever the lease says.

The never-destroy-work checks run FIRST, so no amount of dead-holder or
expired-lease evidence can reach a slot holding work that exists nowhere else.
(WORKTREE_HANDOFF.md counts as dirt — a stopped session's unfinished work is a
busy signal, see GIT_TREE_DISCIPLINE.md §3b.)

Reclaim lives inside `acquire`'s pool-full path rather than in its own verb:
the only moment anyone cares that a finished slot is still held is the moment
the ring is empty, so checking there is free and needs no operator. `release`
stays the manual path — and now reports what the slot IS afterwards, because a
dirty tree or unlanded branch pins a slot with no lease at all.

`acquire` REFUSES a branch already checked out in another slot. `git checkout -B`
overrides git's one-worktree-per-branch rule (plain `checkout` refuses) and
resets the branch ref under the other worktree: 2026-07-29, four slots on
lane/wr-p2-replay, one slot's commits stranded in its reflog.

On acquire, a slot whose target/ exceeds TARGET_CAP_GB is wiped before
handoff (stale artifacts of dead branches otherwise accumulate without
bound) — an occasional cold build in exchange for a hard per-slot disk
ceiling. Worst-case pool size: MAX_SLOTS × TARGET_CAP_GB plus checkouts,
roughly 270 GB (cap raised 6→10 on 2026-07-17, Peter's call — slots are
created on demand, so the pool only reaches this if 10 concurrent
workstreams actually happen).

Release is an optimization, not a safety mechanism: a forgotten lease expires
after LEASE_TTL_HOURS, or sooner if its `holder_pid` is provably gone. Nothing
can be made to release on session end — a killed session fires no hook, and
that is the population that leaks — so the lease records a pid to probe
instead of trusting anyone to clean up.

`scrub` is the end-of-session counterpart to acquire's lazy cap: acquire
only wipes the ONE slot it hands out, so a finished wave leaves every
other slot's warm target on disk until some future acquire happens to
pick it (2026-07-29: ten idle landed slots, 201 GB, SessionStart alarm).
Scrub touches idle slots only (leased, dirty, unlanded, or live-session
slots are skipped): wipe any target/ over TARGET_CAP_GB, then, while the
pool exceeds SCRUB_TO_GB, wipe the least-recently-built idle targets so
the warmest caches survive. A SessionEnd hook runs it automatically
(.claude/hooks/session-end-worktree-scrub.py); by hand it is always safe.

Acquire also drops a `.metadata_never_index` marker at the pool root so
Spotlight never indexes the slot target/ dirs (BUG-297 machine-lockup
relief — see ensure_spotlight_exclusion).
"""

import argparse
import errno
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

def _main_checkout():
    """Anchor to the MAIN checkout even when this script's copy runs inside a
    worktree — __file__-relative anchoring made a nested pool under the
    caller's worktree (BUG-luo2, 2026-07-25: slot-6/.claude/worktrees/slot-0).
    --git-common-dir points at the main repo's .git from any worktree."""
    out = subprocess.run(
        ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
        capture_output=True, text=True, cwd=Path(__file__).parent,
    )
    if out.returncode != 0:
        sys.exit(f"agent-worktree: cannot resolve main checkout: " + out.stderr)
    return Path(out.stdout.strip()).parent


REPO = _main_checkout()
POOL = REPO / ".claude" / "worktrees"
LEASE_NAME = ".worktree-lease.json"  # gitignored; mtime is the staleness clock
LEASE_TTL_HOURS = 8
DEAD_HOLDER_GRACE_H = 0.5  # a dead holder pid only shortens the TTL to this, never
                           # to zero: a freshly acquired slot is clean+landed (HEAD
                           # is the tip), so a pid probe that reads dead too eagerly
                           # would hand a slot away seconds after someone took it.
MAX_SLOTS = 10         # hard structural cap — there is no override flag
TARGET_CAP_GB = 25     # per-slot target/ ceiling, enforced at acquire
SCRUB_TO_GB = 150      # scrub trims the pool under this — below the sentinel's
                       # 200 GB alarm so a scrubbed pool never alarms
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
    """Returns (age_hours or None, owner, task, holder_pid) — None age = no lease."""
    lease = wt / LEASE_NAME
    if not lease.exists():
        return None, "", "", None
    age_h = (time.time() - lease.stat().st_mtime) / 3600
    try:
        data = json.loads(lease.read_text())
    except (json.JSONDecodeError, OSError):
        data = {}
    return age_h, data.get("owner", "?"), data.get("task", "?"), data.get("holder_pid")


def pid_alive(pid):
    """Existence probe, same shape as workflow-runtime's `holder_alive`: signal 0
    succeeds for a live pid and raises EPERM for one we don't own (also alive)."""
    try:
        os.kill(int(pid), 0)
        return True
    except PermissionError:
        return True
    except (OSError, TypeError, ValueError) as e:
        return getattr(e, "errno", None) == errno.EPERM


def lease_blocks(wt):
    """Does this slot's lease still reserve it? Returns (blocks: bool, why: str).

    A recorded holder pid that is gone shortens the TTL to DEAD_HOLDER_GRACE_H
    rather than clearing it outright — dead-holder evidence is a reason to
    expire sooner, never a licence to skip the never-destroy-work checks that
    run before this."""
    age_h, owner, task, holder_pid = lease_info(wt)
    if age_h is None:
        return False, "no lease"
    if age_h >= LEASE_TTL_HOURS:
        return False, f"lease expired ({age_h:.1f}h > {LEASE_TTL_HOURS}h TTL)"
    if holder_pid is not None and not pid_alive(holder_pid) and age_h >= DEAD_HOLDER_GRACE_H:
        return False, f"holder pid {holder_pid} is gone ({owner}, {age_h:.1f}h)"
    return True, f"leased by {owner} for {task} ({age_h:.1f}h ago)"


def branch_holders():
    """branch name -> [worktree paths checked out on it]. `git checkout -B` does
    NOT respect git's one-worktree-per-branch rule (plain `checkout` does), so
    this is the only thing standing between an acquire and resetting a branch
    ref under someone else's live worktree."""
    out = git(REPO, "worktree", "list", "--porcelain", check=False)
    holders, path = {}, None
    for line in out.stdout.splitlines():
        if line.startswith("worktree "):
            path = Path(line[len("worktree "):])
        elif line.startswith("branch ") and path is not None:
            holders.setdefault(line[len("branch "):].removeprefix("refs/heads/"), []).append(path)
    return holders


# Slot categories. Only IDLE and RECLAIMABLE are ever handed out automatically.
IDLE = "IDLE"
RECLAIMABLE = "RECLAIM"      # finished or duplicated work — safe to return to the ring
IN_USE = "IN-USE"            # a live lease or a live session; wait, don't reclaim
NEEDS_HUMAN = "HUMAN"        # uncommitted work, or the sole holder of unlanded commits


def slot_state(wt, holders=None):
    """Returns (category, reason, remedy). Remedy is the exact command or action
    that frees this slot, so POOL FULL can tell an operator what to do per line.

    Order matters: the never-destroy-work checks (dirty, sole-holder-unlanded)
    come FIRST, so no amount of dead-holder or expired-lease evidence can ever
    reach a slot that is holding work which exists nowhere else."""
    if holders is None:
        holders = branch_holders()
    branch = git(wt, "branch", "--show-current").stdout.strip()

    dirt = git(wt, "status", "--porcelain").stdout.strip()
    if dirt:
        n = len(dirt.splitlines())
        return (NEEDS_HUMAN, f"dirty ({n} paths)",
                f"commit or discard the {n} uncommitted path(s) in {wt}")

    if not is_landed(wt):
        # The branch ref survives an acquire (`checkout -B <new>` never touches
        # the old branch), so the commits are never lost either way. What a
        # second holder proves is that the WORKSTREAM keeps a slot — this one is
        # a `checkout -B` clobber artifact, not somebody's seat.
        others = [p for p in holders.get(branch, []) if p != wt]
        if not others:
            return (NEEDS_HUMAN, f"unlanded branch {branch} (sole holder)",
                    f"land or delete {branch}, or detach this slot to origin/main")
        dupes = ", ".join(p.name for p in others)
        blocked, why = lease_blocks(wt)
        if blocked:
            return (IN_USE, f"{why}; duplicate of {dupes}", f"wait, or release {wt.name}")
        return (RECLAIMABLE, f"clean duplicate of {dupes} on {branch} — {why}",
                f"reclaimed automatically; by hand: release {wt.name}")

    blocked, why = lease_blocks(wt)
    if blocked:
        return IN_USE, why, f"wait for the lease, or release {wt.name}"
    if (wt / LEASE_NAME).exists():
        return (RECLAIMABLE, f"clean, landed, {why}",
                f"reclaimed automatically; by hand: release {wt.name}")
    return IDLE, "idle", "already free"


def idle_state(wt):
    """Back-compat shim: (idle, reason) for callers that only want free-or-not."""
    cat, reason, _ = slot_state(wt)
    return cat in (IDLE, RECLAIMABLE), reason


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
    print(f"SLOT:     {wt.name}  (release with: scripts/agent-worktree.py "
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
    holders = branch_holders()
    for wt in slots:
        cat, reason, _ = slot_state(wt, holders)
        branch = git(wt, "branch", "--show-current").stdout.strip() or "(detached)"
        head = git(wt, "rev-parse", "--short", "HEAD").stdout.strip()
        warm = f"{target_bytes(wt) / 2**30:.1f}G target" if target_bytes(wt) else "cold"
        print(f"{cat:8} {wt.name:8} {branch:40} "
              f"{head}  {warm:14} {reason}")


def ensure_spotlight_exclusion():
    """Keep the whole pool out of Spotlight — idempotent, self-healing.

    Each slot's target/ is tens of GB of Rust build artifacts (BUG-297:
    six 19-25 GB targets churned by concurrent lanes made mds_stores
    re-index continuously, dirtying ~8.6 GB of mds_stores over one
    orchestration window and thrashing the machine). A `.metadata_never_index`
    file at the pool root excludes the entire subtree — every slot, its
    target/, and its checkout — from Spotlight (Apple-documented, no sudo,
    honored for the whole directory tree). It sits ABOVE target/ so
    `cargo clean` never removes it; recreating it here on every acquire
    means it survives the pool dir being deleted/recreated. The marker
    lives in gitignored space, so THIS is its durable source of truth.
    """
    POOL.mkdir(parents=True, exist_ok=True)
    marker = POOL / ".metadata_never_index"
    if not marker.exists():
        marker.write_text("")


def slot_has_live_session(wt):
    """True if any claude/shell process has its cwd inside this slot.

    The ring's idle test (clean + landed + lease-free) can't see a session
    that inherited its worktree outside the ring — reusing such a slot
    branch-switches a live session (BUG-luo2: the lead's own slot-6 was
    handed to a lane mid-session 2026-07-25). Best-effort: any error = not
    live (fail open; the lease remains the primary mechanism)."""
    try:
        ps = subprocess.run(["ps", "-axo", "pid=,comm="],
                            capture_output=True, text=True, timeout=10)
        pids = [ln.split(None, 1)[0] for ln in ps.stdout.splitlines()
                if any(k in ln for k in ("claude", "zsh", "bash", "tmux"))]
        for pid in pids:
            lsof = subprocess.run(["lsof", "-a", "-p", pid, "-d", "cwd", "-Fn"],
                                  capture_output=True, text=True, timeout=5)
            for line in lsof.stdout.splitlines():
                if line.startswith("n") and str(wt) in line[1:]:
                    return True
    except Exception:
        pass
    return False


def pool_full_report(slots, states):
    """Exit loudly, grouped by WHO can free each slot. A flat status list reads as
    N busy agents when it is really N abandoned trees (2026-07-30: ten slots, one
    working agent), so abandoned and in-use never share a group again."""
    groups = [
        (IN_USE, "IN USE — a live holder; wait"),
        (NEEDS_HUMAN, "NEEDS A HUMAN — never reclaimed automatically"),
    ]
    err = lambda s: print(s, file=sys.stderr)  # noqa: E731
    for cat, heading in groups:
        members = [wt for wt in slots if states[wt][0] == cat]
        if not members:
            continue
        err(f"\n{heading}:")
        for wt in members:
            _, reason, remedy = states[wt]
            err(f"  {wt.name}: {reason}")
            err(f"      -> {remedy}")
    dirty = sum(1 for wt in slots if "dirty" in states[wt][1])
    unlanded = sum(1 for wt in slots if "unlanded" in states[wt][1])
    sys.exit(
        f"\nPOOL FULL: {len(slots)}/{MAX_SLOTS} slots, none reclaimable "
        f"({dirty} holding uncommitted work, {unlanded} sole holders of unlanded "
        "commits). The ring never grows past its cap — this failure is deliberate "
        "and loud. Clean or land a slot per the remedies above, wait for a lease "
        f"(TTL {LEASE_TTL_HOURS}h), or surface this to Peter. Do NOT create a "
        "worktree by hand."
    )


def refuse_if_branch_held_elsewhere(branch, chosen, holders):
    """`git checkout -B` silently overrides git's one-worktree-per-branch rule and
    RESETS the branch ref under the other worktree (2026-07-29: four slots on
    lane/wr-p2-replay, slot-7's reflog recording the reset). Plain `checkout`
    refuses this; `-B` must be made to refuse it too."""
    for other in holders.get(branch, []):
        if other != chosen:
            sys.exit(
                f"REFUSED: branch {branch} is already checked out at {other}. "
                f"`checkout -B` would reset that branch ref under a live worktree "
                f"and strand its commits in the reflog. Work in {other.name}, or "
                f"acquire under a different branch name."
            )


def cmd_acquire(args):
    ensure_spotlight_exclusion()
    git(REPO, "fetch", "origin", "main")
    tip = args.tip or "origin/main"
    slots = pool_slots()
    holders = branch_holders()

    states = {wt: slot_state(wt, holders) for wt in slots}
    free = [wt for wt in slots if states[wt][0] in (IDLE, RECLAIMABLE)]
    live = [wt for wt in free if slot_has_live_session(wt)]
    if live:
        free = [wt for wt in free if wt not in live]
        for wt in live:
            print(f"SKIP {wt.name}: a live session is cd'd inside it "
                  "(reusing it would switch that session's branch mid-flight — "
                  "BUG-luo2)", file=sys.stderr)
    if free:
        # Reclaim happens HERE rather than in a separate verb: a slot that is
        # clean and landed (or a clean duplicate of a branch another slot holds)
        # is finished work, and the only moment anyone cares is the moment the
        # ring is empty — so the check is free and needs no operator.
        wt = max(free, key=target_bytes)  # warmest target = best build reuse
        if states[wt][0] == RECLAIMABLE:
            print(f"RECLAIM {wt.name}: {states[wt][1]}")
        refuse_if_branch_held_elsewhere(args.branch, wt, holders)
        enforce_target_cap(wt)
        git(wt, "checkout", "-B", args.branch, tip)
        print(f"REUSED {wt.name} ({target_bytes(wt) / 2**30:.1f}G warm target)")
    elif len(slots) < MAX_SLOTS:
        refuse_if_branch_held_elsewhere(args.branch, None, holders)
        # Fill the lowest free index so slot names stay dense.
        taken = {wt.name for wt in slots}
        idx = next(i for i in range(MAX_SLOTS)
                   if f"{SLOT_PREFIX}{i}" not in taken)
        wt = POOL / f"{SLOT_PREFIX}{idx}"
        git(REPO, "worktree", "add", "-b", args.branch, str(wt), tip)
        print(f"CREATED {wt.name} (ring at {len(slots) + 1}/{MAX_SLOTS} — "
              "cold build ahead)")
    else:
        pool_full_report(slots, states)

    # holder_pid makes the lease self-describing: liveness becomes a pid probe
    # instead of an 8h timeout. Default is the CALLER's pid (this script exits
    # immediately, so its own pid would read dead at once) — a shell that exits
    # is a false "dead", which is exactly why DEAD_HOLDER_GRACE_H exists.
    (wt / LEASE_NAME).write_text(json.dumps(
        {"owner": args.owner, "task": args.name, "branch": args.branch,
         "holder_pid": args.holder_pid if args.holder_pid is not None else os.getppid(),
         "acquired": time.strftime("%Y-%m-%dT%H:%M:%S%z")}) + "\n")
    copied = copy_missing_fixtures(wt)
    print(f"FIXTURES: {copied} gitignored file(s) copied from main checkout")
    verify_and_report(wt)


def build_recency(wt):
    """Best available 'last build' clock for LRU scrub ordering: newest mtime
    among target/'s immediate children (cargo touches debug/ or release/ on
    every build; the target/ root mtime only moves when entries appear)."""
    t = wt / "target"
    times = [t.stat().st_mtime] if t.is_dir() else [0.0]
    if t.is_dir():
        times += [p.stat().st_mtime for p in t.iterdir()]
    return max(times)


def cmd_scrub(_args):
    slots = pool_slots()
    holders = branch_holders()
    idle = []
    for wt in slots:
        cat, reason, _ = slot_state(wt, holders)
        if cat not in (IDLE, RECLAIMABLE):
            print(f"KEEP {wt.name}: {reason}")
        elif slot_has_live_session(wt):
            print(f"KEEP {wt.name}: live session inside it")
        else:
            idle.append(wt)
    for wt in idle:
        enforce_target_cap(wt)

    def pool_gb():
        out = subprocess.run(["du", "-sk", str(POOL)],
                             capture_output=True, text=True)
        return int(out.stdout.split()[0]) / 2**20 if out.returncode == 0 else 0

    total = pool_gb()
    victims = sorted((wt for wt in idle if target_bytes(wt)), key=build_recency)
    while total > SCRUB_TO_GB and victims:
        wt = victims.pop(0)  # least recently built loses its cache first
        size = target_bytes(wt) / 2**30
        shutil.rmtree(wt / "target", ignore_errors=True)
        print(f"SCRUBBED {wt.name}: {size:.1f}G target wiped (pool over "
              f"{SCRUB_TO_GB}G)")
        total = pool_gb()
    print(f"POOL: {total:.0f}G ({len(idle)} idle / {len(slots)} slots, "
          f"scrub target {SCRUB_TO_GB}G)")


def cmd_release(args):
    """Dropping the lease is only ONE of the things that can pin a slot — a dirty
    tree or an unlanded branch pins it with no lease at all, and the old
    "nothing to do" left an operator staring at a slot that stayed unusable
    (Peter, 2026-07-30). Always report what the slot is after the drop."""
    wt = POOL / args.slot
    if not wt.is_dir():
        sys.exit(f"no slot at {wt}")
    lease = wt / LEASE_NAME
    if lease.exists():
        lease.unlink()
        print(f"released {wt}")
    else:
        print(f"no lease on {wt}")
    cat, reason, remedy = slot_state(wt)
    print(f"{cat}: {reason}")
    if cat not in (IDLE, RECLAIMABLE):
        print(f"  -> still pinned. {remedy}")


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
    acq.add_argument("--holder-pid", type=int, default=None, dest="holder_pid",
                     help="pid whose death expires this lease early (default: "
                          "the calling process)")
    rel = sub.add_parser("release")
    rel.add_argument("slot", help="slot name printed by acquire (e.g. slot-2)")
    sub.add_parser("scrub")
    args = parser.parse_args()
    {"list": cmd_list, "acquire": cmd_acquire, "release": cmd_release,
     "scrub": cmd_scrub}[args.cmd](args)


if __name__ == "__main__":
    main()
