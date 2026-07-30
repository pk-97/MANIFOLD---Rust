#!/usr/bin/env python3
"""Standalone test runner for agent-worktree.py's slot categories.

The categories are git-derived, so these build REAL throwaway repos under a
temp dir and repoint the module's REPO/POOL globals at them. Nothing touches
the live pool. Same PASS/FAIL shape as .claude/hooks/test_*.py.

Run: python3 scripts/test_agent_worktree.py
"""
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import time
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "agent-worktree.py"

spec = importlib.util.spec_from_file_location("agent_worktree", SCRIPT)
aw = importlib.util.module_from_spec(spec)
spec.loader.exec_module(aw)

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


def sh(cwd, *args):
    out = subprocess.run(args, cwd=str(cwd), capture_output=True, text=True)
    if out.returncode != 0:
        raise RuntimeError(f"{args} in {cwd}: {out.stderr}")
    return out.stdout.strip()


def build_pool(tmp):
    """An origin, a main checkout tracking it, and an empty slot pool."""
    tmp = tmp.resolve()  # macOS /var -> /private/var; git reports resolved paths
    origin, repo = tmp / "origin.git", tmp / "main"
    sh(tmp, "git", "init", "-q", "--bare", "-b", "main", str(origin))
    sh(tmp, "git", "init", "-q", "-b", "main", str(repo))
    sh(repo, "git", "config", "user.email", "t@t")
    sh(repo, "git", "config", "user.name", "t")
    (repo / "f.txt").write_text("base\n")
    # Mirrors the live .gitignore. The bare lease line is load-bearing: inside a
    # slot the lease sits at the WORKTREE root, so `.claude/*` never matches it
    # and every leased slot would read as dirty.
    (repo / ".gitignore").write_text(".worktree-lease.json\n.claude/*\n")
    sh(repo, "git", "add", "f.txt", ".gitignore")
    sh(repo, "git", "commit", "-qm", "base")
    sh(repo, "git", "remote", "add", "origin", str(origin))
    sh(repo, "git", "push", "-q", "origin", "main")
    sh(repo, "git", "fetch", "-q", "origin", "main")
    aw.REPO, aw.POOL = repo, repo / ".claude" / "worktrees"
    aw.POOL.mkdir(parents=True)
    return repo


def add_slot(repo, name, branch, tip="origin/main"):
    wt = aw.POOL / name
    sh(repo, "git", "worktree", "add", "-q", "-b", branch, str(wt), tip)
    return wt


def write_lease(wt, owner="unnamed-session", task="t", holder_pid=None, age_h=0.0):
    (wt / aw.LEASE_NAME).write_text(json.dumps(
        {"owner": owner, "task": task, "holder_pid": holder_pid}) + "\n")
    if age_h:
        old = time.time() - age_h * 3600
        os.utime(wt / aw.LEASE_NAME, (old, old))


def dead_pid():
    """A pid guaranteed not to exist: fork a child and reap it."""
    p = subprocess.Popen([sys.executable, "-c", "pass"])
    p.wait()
    return p.pid


# ---------------------------------------------------------------- categories

def test_clean_landed_no_lease_is_idle(repo):
    wt = add_slot(repo, "slot-0", "lane/a")
    cat, reason, _ = aw.slot_state(wt)
    check("clean+landed, no lease -> IDLE", cat == aw.IDLE, f"{cat}: {reason}")


def test_clean_landed_stale_lease_is_reclaimable(repo):
    wt = add_slot(repo, "slot-1", "lane/b")
    write_lease(wt, age_h=aw.LEASE_TTL_HOURS + 1)
    cat, reason, remedy = aw.slot_state(wt)
    check("clean+landed, expired lease -> RECLAIM", cat == aw.RECLAIMABLE, f"{cat}: {reason}")
    check("reclaim remedy is automatic", "automatic" in remedy, remedy)


def test_clean_landed_live_lease_is_in_use(repo):
    wt = add_slot(repo, "slot-2", "lane/c")
    write_lease(wt, holder_pid=os.getpid(), age_h=1.0)
    cat, reason, _ = aw.slot_state(wt)
    check("clean+landed, live lease -> IN-USE", cat == aw.IN_USE, f"{cat}: {reason}")


def test_dirty_is_never_reclaimable(repo):
    wt = add_slot(repo, "slot-3", "lane/d")
    (wt / "f.txt").write_text("uncommitted\n")
    write_lease(wt, holder_pid=dead_pid(), age_h=aw.LEASE_TTL_HOURS + 99)
    cat, reason, remedy = aw.slot_state(wt)
    check("dirty + dead holder + expired lease -> HUMAN",
          cat == aw.NEEDS_HUMAN, f"{cat}: {reason}")
    check("dirty reason counts paths", "dirty (1 paths)" in reason, reason)
    check("dirty remedy names commit-or-discard",
          "commit or discard" in remedy, remedy)


def test_unlanded_sole_holder_is_never_reclaimable(repo):
    wt = add_slot(repo, "slot-4", "lane/e")
    (wt / "f.txt").write_text("work\n")
    sh(wt, "git", "add", "f.txt")
    sh(wt, "git", "commit", "-qm", "unlanded work")
    write_lease(wt, holder_pid=dead_pid(), age_h=aw.LEASE_TTL_HOURS + 99)
    cat, reason, remedy = aw.slot_state(wt)
    check("clean but unlanded sole holder -> HUMAN",
          cat == aw.NEEDS_HUMAN, f"{cat}: {reason}")
    check("unlanded reason says sole holder", "sole holder" in reason, reason)
    check("unlanded remedy names land-or-delete", "land or delete" in remedy, remedy)


def test_unlanded_duplicate_is_reclaimable(repo):
    """The wr-p2-replay case: `checkout -B` put one branch in several slots."""
    wt = add_slot(repo, "slot-5", "lane/dup")
    (wt / "f.txt").write_text("shared work\n")
    sh(wt, "git", "add", "f.txt")
    sh(wt, "git", "commit", "-qm", "unlanded shared work")
    twin = aw.POOL / "slot-6"
    sh(repo, "git", "worktree", "add", "-q", "--detach", str(twin), "origin/main")
    sh(twin, "git", "checkout", "-B", "lane/dup", "lane/dup")  # the -B clobber
    write_lease(twin, age_h=aw.LEASE_TTL_HOURS + 1)
    cat, reason, _ = aw.slot_state(twin)
    check("clean duplicate of an unlanded branch -> RECLAIM",
          cat == aw.RECLAIMABLE, f"{cat}: {reason}")
    check("duplicate reason names the other slot", "slot-5" in reason, reason)
    # Reclaim takes ONE slot per acquire, and that is what stops it taking the
    # last copy: once the twin is repointed the original is the sole holder
    # again, so the next acquire sees NEEDS_HUMAN rather than a second spare.
    sh(twin, "git", "checkout", "-qB", "feat/reclaimed", "origin/main")
    cat_o, reason_o, _ = aw.slot_state(wt)
    check("after one duplicate is reclaimed the last copy needs a human",
          cat_o == aw.NEEDS_HUMAN and "sole holder" in reason_o, f"{cat_o}: {reason_o}")
    check("the reclaimed twin kept the branch ref intact",
          sh(repo, "git", "rev-parse", "lane/dup") ==
          sh(wt, "git", "rev-parse", "HEAD"), "branch ref moved")


def test_dead_holder_grace_protects_a_fresh_acquire(repo):
    """A just-acquired slot is clean AND landed; a too-eager pid probe would hand
    it straight back out."""
    wt = add_slot(repo, "slot-7", "lane/fresh")
    write_lease(wt, holder_pid=dead_pid(), age_h=0.0)
    cat, reason, _ = aw.slot_state(wt)
    check("dead holder inside the grace window -> IN-USE",
          cat == aw.IN_USE, f"{cat}: {reason}")
    write_lease(wt, holder_pid=dead_pid(), age_h=aw.DEAD_HOLDER_GRACE_H + 0.1)
    cat, reason, _ = aw.slot_state(wt)
    check("dead holder past the grace window -> RECLAIM",
          cat == aw.RECLAIMABLE, f"{cat}: {reason}")
    check("dead-holder reason names the pid", "holder pid" in reason, reason)


# ------------------------------------------------------- POOL FULL reporting

def test_pool_full_groups_each_slot_correctly(repo):
    dirty = add_slot(repo, "slot-0", "lane/dirty")
    (dirty / "f.txt").write_text("uncommitted\n")
    unlanded = add_slot(repo, "slot-1", "lane/unlanded")
    (unlanded / "g.txt").write_text("x\n")
    sh(unlanded, "git", "add", "g.txt")
    sh(unlanded, "git", "commit", "-qm", "unlanded")
    busy = add_slot(repo, "slot-2", "lane/busy")
    write_lease(busy, owner="lead", task="live-work", holder_pid=os.getpid(), age_h=1.0)

    slots = [dirty, unlanded, busy]
    states = {wt: aw.slot_state(wt) for wt in slots}
    err = io.StringIO()
    code = None
    try:
        with redirect_stderr(err), redirect_stdout(io.StringIO()):
            aw.pool_full_report(slots, states)
    except SystemExit as e:
        code = e.code
    text = err.getvalue() + str(code)

    check("POOL FULL exits nonzero", code not in (0, None), repr(code))
    check("POOL FULL has an IN USE group", "IN USE" in text, text)
    check("POOL FULL has a NEEDS A HUMAN group", "NEEDS A HUMAN" in text, text)
    check("in-use slot is not filed as needing a human",
          text.index("IN USE") < text.index("NEEDS A HUMAN"), text)
    check("dirty slot names its remedy", "commit or discard" in text, text)
    check("unlanded slot names its remedy", "land or delete" in text, text)
    check("summary counts the dirty slot", "1 holding uncommitted work" in text, text)
    check("summary counts the unlanded slot", "1 sole holders" in text, text)
    check("live lease is named, not just 'busy'", "lead" in text and "live-work" in text, text)


# ------------------------------------------------------ checkout -B refusal

def test_acquire_refuses_a_branch_held_elsewhere(repo):
    held = add_slot(repo, "slot-0", "lane/held")
    holders = aw.branch_holders()
    check("branch_holders sees the slot", held in holders.get("lane/held", []), str(holders))
    code = None
    err = io.StringIO()
    try:
        with redirect_stderr(err), redirect_stdout(io.StringIO()):
            aw.refuse_if_branch_held_elsewhere("lane/held", aw.POOL / "slot-9", holders)
    except SystemExit as e:
        code = e.code
    check("acquiring a branch held elsewhere is refused", code not in (0, None), repr(code))
    check("refusal names the holding slot", "slot-0" in str(code), str(code))


def test_acquire_allows_the_slot_that_already_holds_it(repo):
    held = add_slot(repo, "slot-0", "lane/held")
    try:
        aw.refuse_if_branch_held_elsewhere("lane/held", held, aw.branch_holders())
        ok = True
    except SystemExit as e:
        ok, detail = False, str(e)
    check("re-acquiring into the same slot is allowed", ok, locals().get("detail", ""))


# ---------------------------------------------------------------------- main

TESTS = [
    test_clean_landed_no_lease_is_idle,
    test_clean_landed_stale_lease_is_reclaimable,
    test_clean_landed_live_lease_is_in_use,
    test_dirty_is_never_reclaimable,
    test_unlanded_sole_holder_is_never_reclaimable,
    test_unlanded_duplicate_is_reclaimable,
    test_dead_holder_grace_protects_a_fresh_acquire,
    test_pool_full_groups_each_slot_correctly,
    test_acquire_refuses_a_branch_held_elsewhere,
    test_acquire_allows_the_slot_that_already_holds_it,
]


def main():
    for fn in TESTS:
        with tempfile.TemporaryDirectory() as tmp:  # one clean pool per test
            try:
                fn(build_pool(Path(tmp)))
            except Exception as e:  # a crashing test is a failing test
                FAIL.append((fn.__name__, f"raised {e!r}"))

    for name in PASS:
        print(f"PASS: {name}")
    for name, detail in FAIL:
        print(f"FAIL: {name} ({detail!r})")
    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
