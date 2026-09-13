#!/usr/bin/env python3
"""Reusable worktree ring with verified archival and bounded inactive caches.

Commands: list; acquire TASK NEW_BRANCH; release SLOT; retire SLOT [--include FILE]; scrub.
Acquire reuses clean landed slots, or clean inactive branches whose exact HEAD
is freshly confirmed on origin. It never resets an existing branch name.
Process inspection failures protect the checkout. The ring remains capped at ten.

Retire preserves reviewed tracked changes and handoff notes on a unique remote
archive/worktrees branch, verifies its SHA, then clears the checkout and cache.
Unknown untracked paths require explicit inclusion. Staged changes and merges
must be resolved first. Failed uploads preserve source and a local archive.
Unique ignored assets remain local: preserve these before removing a checkout.
Public remotes expose archive contents. Archives are not verified app landings.

Acquire and release scrub inactive caches toward 40 GiB, with 25 GiB per idle
slot. Active caches are protected; these are cleanup budgets, not build limits.
Successful landings release their slot. Fixture copying prunes hidden and target
subtrees so old quarantine fixtures cannot multiply across the pool.

Confirm the printed acquired HEAD before editing. Never bypass the slot cap.
"""

import argparse
import errno
import json
import os
import shutil
import subprocess
import sys
import time
import uuid
import tempfile
import hashlib
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
SCRUB_TO_GB = 40      # scrub trims the pool under this — below the sentinel's
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


def remote_contains_head(wt, branch):
    head = git(wt, "rev-parse", "HEAD", check=False)
    remote = subprocess.run(["git", "-C", str(REPO), "ls-remote", "origin",
                             f"refs/heads/{branch}"], capture_output=True, text=True, timeout=10)
    if head.returncode or remote.returncode != 0:
        return False
    fields = remote.stdout.split()
    return bool(fields) and fields[0] == head.stdout.strip()


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
    """Existence probe: signal 0 succeeds for a live pid and raises EPERM
    for one we don't own (also alive)."""
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
    for root, dirs, files in os.walk(REPO):
        dirs[:] = [d for d in dirs if not d.startswith(".") and d != "target"]
        parts = Path(root).relative_to(REPO).parts
        if not any(parts[i:i + 2] == ("tests", "fixtures") for i in range(len(parts) - 1)):
            continue
        for name in files:
            src = Path(root) / name
            if not (wt / src.relative_to(REPO)).exists():
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
    try:
        out = subprocess.run(["lsof", "-n", "-P", "-a", "-d", "cwd", "-Fpn"],
                             capture_output=True, text=True, timeout=20)
        if out.returncode != 0 or not out.stdout:
            return True
        root = wt.resolve()
        for line in out.stdout.splitlines():
            if line.startswith("n"):
                cwd = Path(line[1:]).resolve()
                if cwd == root or root in cwd.parents:
                    return True
    except (OSError, RuntimeError, subprocess.TimeoutExpired):
        return True
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


def refuse_if_branch_ref_exists(branch, chosen, holders):
    if git(REPO, "show-ref", "--verify", f"refs/heads/{branch}", check=False).returncode != 0:
        return
    sys.exit(f"REFUSED: branch {branch} already exists; choose a new branch name")


def cmd_acquire(args):
    ensure_spotlight_exclusion()
    cmd_scrub(args)
    git(REPO, "fetch", "origin", "main")
    tip = args.tip or "origin/main"
    slots = pool_slots()
    holders = branch_holders()

    states = {wt: slot_state(wt, holders) for wt in slots}
    free = [wt for wt in slots if states[wt][0] in (IDLE, RECLAIMABLE)]
    # A clean sole-holder branch may be reused only after its remote backup is verified.
    for wt in slots:
        if states[wt][0] == NEEDS_HUMAN and not git(wt, "status", "--porcelain").stdout.strip() and not lease_blocks(wt)[0] and not slot_has_live_session(wt):
            branch = git(wt, "branch", "--show-current").stdout.strip()
            if branch and remote_contains_head(wt, branch):
                free.append(wt)
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
        refuse_if_branch_ref_exists(args.branch, wt, holders)
        enforce_target_cap(wt)
        git(wt, "checkout", "-B", args.branch, tip)
        print(f"REUSED {wt.name} ({target_bytes(wt) / 2**30:.1f}G warm target)")
    elif len(slots) < MAX_SLOTS:
        refuse_if_branch_held_elsewhere(args.branch, None, holders)
        refuse_if_branch_ref_exists(args.branch, None, holders)
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
    idle, pinned = [], []
    for wt in slots:
        cat, reason, _ = slot_state(wt, holders)
        if cat == IN_USE:
            print(f"KEEP {wt.name}: {reason}")
        elif slot_has_live_session(wt):
            print(f"KEEP {wt.name}: live session inside it")
        elif cat in (IDLE, RECLAIMABLE):
            idle.append(wt)
        else:
            blocked, why = lease_blocks(wt)
            if blocked:
                # A live lease is a contract — even a dirty tree doesn't make
                # its cache fair game until the lease expires.
                print(f"KEEP {wt.name}: {why}")
            else:
                pinned.append((wt, reason))
    # Pinned slots (dirty tree, sole holder of unlanded commits) keep their
    # checkout and branch; only the cargo cache leaves disk. Wiping target/
    # cannot lose work — it is rebuilt from source on the next build.
    for wt, reason in pinned:
        size = target_bytes(wt)
        if size:
            shutil.rmtree(wt / "target", ignore_errors=True)
            print(f"CACHE-ONLY {wt.name}: {size / 2**30:.1f}G target wiped, "
                  f"work untouched (pinned: {reason})")
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
    slots = pool_slots()
    if wt not in slots or wt.is_symlink():
        sys.exit(f"REFUSED: invalid slot: {args.slot}")
    lease = wt / LEASE_NAME
    if lease.exists():
        lease.unlink()
        print(f"released {wt}")
    else:
        print(f"no lease on {wt}")
    cat, reason, remedy = slot_state(wt)
    print(f"{cat}: {reason}")
    cmd_scrub(args)
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
    ret = sub.add_parser("retire")
    ret.add_argument("slot")
    ret.add_argument("--include", action="append", default=[],
                     help="explicitly preserve an otherwise unknown untracked path")
    rem = sub.add_parser("remove", help="remove a clean backed-up inactive checkout")
    rem.add_argument("slot", help="slot name or exact registered worktree path")
    rem.add_argument("--recovery", type=Path, help="local recovery archive with blobs and ignored-files.json")
    sub.add_parser("scrub")
    args = parser.parse_args()
    {"list": cmd_list, "acquire": cmd_acquire, "release": cmd_release,
     "retire": cmd_retire, "remove": cmd_remove, "scrub": cmd_scrub}[args.cmd](args)



def _safe_rel(path, wt):
    p = Path(path)
    if p.is_absolute() or ".." in p.parts:
        sys.exit(f"REFUSED: path escapes slot: {path}")
    resolved = (wt / p).resolve()
    if wt.resolve() not in (resolved, *resolved.parents):
        sys.exit(f"REFUSED: path escapes slot: {path}")
    return p


def cmd_remove(args):
    wt = Path(args.slot) if Path(args.slot).is_absolute() else POOL / args.slot
    registered = {Path(line[9:]) for line in git(REPO, "worktree", "list", "--porcelain").stdout.splitlines()
                  if line.startswith("worktree ")}
    if wt not in registered or wt.resolve() == REPO.resolve() or wt.is_symlink():
        sys.exit("REFUSED: not an eligible registered worktree")
    if lease_blocks(wt)[0] or slot_has_live_session(wt):
        sys.exit("REFUSED: worktree is active")
    if git(wt, "status", "--porcelain").stdout:
        sys.exit("REFUSED: retire dirty work before removing its checkout")
    branch = git(wt, "branch", "--show-current").stdout.strip()
    if not is_landed(wt) and not (branch and remote_contains_head(wt, branch)):
        sys.exit("REFUSED: HEAD has no verified remote backup")

    def digest(path):
        with path.open("rb") as stream:
            return hashlib.file_digest(stream, "sha256").hexdigest()

    records = {}
    if args.recovery:
        records = {(item["slot"], item["path"]): item["sha256"] for item in
                   json.loads((args.recovery / "ignored-files.json").read_text())}
    ignored = git(wt, "ls-files", "--others", "--ignored", "--exclude-standard", "-z").stdout.split("\0")
    for name in filter(None, ignored):
        parts = Path(name).parts
        if "target" in parts or "__pycache__" in parts or name.endswith(".DS_Store") or name == LEASE_NAME:
            continue
        source = wt / name
        if source.is_symlink():
            sys.exit(f"REFUSED: preserve ignored symlink explicitly: {name}")
        sha = digest(source)
        main_copy = REPO / name
        if main_copy.is_file() and digest(main_copy) == sha:
            continue
        if (args.recovery and records.get((wt.name, name)) == sha
                and (args.recovery / "blobs" / sha).is_file()
                and digest(args.recovery / "blobs" / sha) == sha):
            continue
        sys.exit(f"REFUSED: unique ignored file lacks verified recovery copy: {name}")
    if slot_has_live_session(wt) or git(wt, "status", "--porcelain").stdout:
        sys.exit("REFUSED: checkout became active or dirty during inspection")
    git(REPO, "worktree", "remove", str(wt))
    print(f"REMOVED {wt}; branch history preserved")


def cmd_retire(args):
    wt = POOL / args.slot
    if wt not in pool_slots() or wt.is_symlink():
        sys.exit(f"REFUSED: invalid slot: {args.slot}")
    blocked, why = lease_blocks(wt)
    if blocked or slot_has_live_session(wt):
        sys.exit(f"REFUSED: {wt.name} is active ({why if blocked else 'live session'})")
    branch = git(wt, "branch", "--show-current").stdout.strip()
    head = git(wt, "rev-parse", "HEAD").stdout.strip()
    if not branch:
        sys.exit("REFUSED: slot is detached")
    if git(wt, "rev-parse", "--verify", "MERGE_HEAD", check=False).returncode == 0:
        sys.exit("REFUSED: unfinished merge")
    if git(wt, "diff", "--cached", "--name-only").stdout:
        sys.exit("REFUSED: staged changes; finish the staged commit before retirement")

    def names(*args):
        return [p for p in git(wt, *args, "-z").stdout.split("\0") if p]

    tracked = names("diff", "--no-renames", "--name-only", "HEAD")
    unknown = names("ls-files", "--others", "--exclude-standard")
    includes = {str(_safe_rel(p, wt)) for p in (args.include or [])}
    bad = set(unknown) - includes - {"WORKTREE_HANDOFF.md"}
    if bad:
        sys.exit("REFUSED: unknown untracked paths: " + ", ".join(sorted(bad)) + "; use --include PATH")
    if includes - set(unknown):
        sys.exit("REFUSED: --include must name exact non-ignored untracked files")
    paths = sorted(set(tracked) | set(unknown))
    for path in paths:
        _safe_rel(path, wt)
    archive = f"archive/worktrees/{time.strftime('%Y-%m-%d')}/{wt.name}-{branch.replace('/', '-')}-{uuid.uuid4().hex[:12]}"
    with tempfile.TemporaryDirectory(prefix="manifold-retire-") as tmp:
        env = dict(os.environ, GIT_INDEX_FILE=str(Path(tmp) / "index"), GIT_LITERAL_PATHSPECS="1")

        def indexed(*args):
            return subprocess.run(["git", "-C", str(wt), *args], env=env,
                                  capture_output=True, text=True, check=True).stdout.strip()

        def snapshot_tree():
            indexed("read-tree", head)
            if paths:
                indexed("add", "--", *paths)
            return indexed("write-tree")

        tree = snapshot_tree()
        message = f"Archive unfinished work from {wt.name}\n\nOriginal branch: {branch}\nOriginal HEAD: {head}\nUnverified archival snapshot; not an app landing.\n"
        commit = subprocess.run(["git", "-C", str(wt), "commit-tree", tree, "-p", head],
                                input=message, capture_output=True, text=True, check=True).stdout.strip()
        git(REPO, "update-ref", f"refs/heads/{archive}", commit, "0" * 40)
        # Retain the local archive even when the network fails.
        git(REPO, "push", "origin", f"{commit}:refs/heads/{archive}")
        remote = git(REPO, "ls-remote", "--heads", "origin", f"refs/heads/{archive}").stdout.split()
        if remote != [commit, f"refs/heads/{archive}"]:
            sys.exit("REFUSED: archive SHA verification failed; source and local archive retained")
        if (git(wt, "rev-parse", "HEAD").stdout.strip() != head
                or git(wt, "diff", "--cached", "--name-only").stdout
                or names("diff", "--no-renames", "--name-only", "HEAD") != tracked
                or names("ls-files", "--others", "--exclude-standard") != unknown
                or snapshot_tree() != tree
                or slot_has_live_session(wt)):
            sys.exit("REFUSED: checkout changed during archival; source retained")
        # Adopt the verified snapshot index without rewriting the original branch.
        # The worktree already matches this tree byte-for-byte.
        git(wt, "read-tree", commit)
        git(wt, "checkout", "--detach", commit)
        if git(wt, "status", "--porcelain").stdout:
            sys.exit("REFUSED: archive checkout is not clean; preserved for inspection")
        git(wt, "checkout", "--detach", "origin/main")
        (wt / LEASE_NAME).unlink(missing_ok=True)
        target = wt / "target"
        if target.is_symlink():
            sys.exit("REFUSED: target is a symlink; archive is safe, cache untouched")
        if target.exists():
            shutil.rmtree(target)
        print(f"RETIRED {wt.name}: {branch} preserved as {archive} at {commit}")


if __name__ == "__main__":
    main()
