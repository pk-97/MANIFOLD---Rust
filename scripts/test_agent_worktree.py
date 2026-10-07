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
import shutil
import subprocess
import sys
import tempfile
import time
import textwrap
import threading
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

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
    (repo / ".gitignore").write_text(".worktree-lease.json\n.claude/*\ntarget/\n")
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


def subprocess_module_code(repo, argv, body):
    """Load the script in a child, then point it at this test's temporary pool."""
    prefix = textwrap.dedent(f"""
        import importlib.util
        import sys
        import time
        from pathlib import Path
        spec = importlib.util.spec_from_file_location("agent_worktree", {str(SCRIPT)!r})
        aw = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(aw)
        aw.REPO = Path({str(repo)!r})
        aw.POOL = aw.REPO / ".claude" / "worktrees"
        sys.argv = ["agent-worktree.py", *{argv!r}]
    """)
    return prefix + body + "\n"


def run_module_subprocess(repo, argv, body="aw.main()", **kwargs):
    return subprocess.run(
        [sys.executable, "-B", "-c", subprocess_module_code(repo, argv, body)],
        cwd=str(repo), capture_output=True, text=True, **kwargs)


def start_lock_holder(repo):
    body = 'with aw.pool_lock():\n    print("LOCKED", flush=True)\n    time.sleep(30)'
    process = subprocess.Popen(
        [sys.executable, "-B", "-c", subprocess_module_code(repo, [], body)],
        cwd=str(repo), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if process.stdout.readline().strip() != "LOCKED":
        stderr = process.stderr.read()
        process.kill()
        process.wait()
        raise RuntimeError(f"lock holder failed to start: {stderr}")
    return process


# ---------------------------------------------------------------- categories

def test_clean_landed_no_lease_is_idle(repo):
    wt = add_slot(repo, "slot-0", "lane/a")
    cat, reason, _ = aw.slot_state(wt)
    check("clean+landed, no lease -> IDLE", cat == aw.IDLE, f"{cat}: {reason}")


def test_clean_landed_old_lease_stays_in_use(repo):
    """BUG-wznn: no lease age frees a slot; only `release` does."""
    wt = add_slot(repo, "slot-1", "lane/b")
    write_lease(wt, age_h=99.0)
    cat, reason, remedy = aw.slot_state(wt)
    check("clean+landed, 99h-old lease -> IN-USE", cat == aw.IN_USE, f"{cat}: {reason}")
    check("old-lease remedy is release", "release slot-1" in remedy, remedy)


def test_clean_landed_live_lease_is_in_use(repo):
    wt = add_slot(repo, "slot-2", "lane/c")
    write_lease(wt, holder_pid=os.getpid(), age_h=1.0)
    cat, reason, _ = aw.slot_state(wt)
    check("clean+landed, live lease -> IN-USE", cat == aw.IN_USE, f"{cat}: {reason}")


def test_live_holder_old_lease_stays_in_use(repo):
    wt = add_slot(repo, "slot-2", "lane/live-old")
    write_lease(wt, owner="lead", task="still-working", holder_pid=os.getpid(),
                age_h=99.0)
    blocked, reason = aw.lease_blocks(wt)
    check("live holder, old lease still blocks", blocked, reason)
    check("reason names holder", "holder pid" in reason and "alive" in reason, reason)


def test_dirty_is_never_reclaimable(repo):
    wt = add_slot(repo, "slot-3", "lane/d")
    (wt / "f.txt").write_text("uncommitted\n")
    write_lease(wt, holder_pid=dead_pid(), age_h=99.0)
    cat, reason, remedy = aw.slot_state(wt)
    check("dirty + dead holder + old lease -> HUMAN",
          cat == aw.NEEDS_HUMAN, f"{cat}: {reason}")
    check("dirty reason counts paths", "dirty (1 paths)" in reason, reason)
    check("dirty remedy names commit-or-discard",
          "commit or discard" in remedy, remedy)


def test_unlanded_sole_holder_is_never_reclaimable(repo):
    wt = add_slot(repo, "slot-4", "lane/e")
    (wt / "f.txt").write_text("work\n")
    sh(wt, "git", "add", "f.txt")
    sh(wt, "git", "commit", "-qm", "unlanded work")
    write_lease(wt, holder_pid=dead_pid(), age_h=99.0)
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
    sh(twin, "git", "symbolic-ref", "HEAD", "refs/heads/lane/dup")  # duplicate holder fixture
    sh(twin, "git", "read-tree", "-m", "-u", "lane/dup")
    cat, reason, _ = aw.slot_state(twin)
    check("clean duplicate of an unlanded branch -> RECLAIM",
          cat == aw.RECLAIMABLE, f"{cat}: {reason}")
    write_lease(twin, age_h=99.0)
    cat_l, reason_l, _ = aw.slot_state(twin)
    check("a leased duplicate is IN-USE, not RECLAIM",
          cat_l == aw.IN_USE, f"{cat_l}: {reason_l}")
    (twin / aw.LEASE_NAME).unlink()
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


def test_dead_holder_never_frees_a_lease(repo):
    """The caller's shell exits after every tool call, so holder_pid always reads
    dead between calls; that must not free a working lane's slot (BUG-wznn)."""
    wt = add_slot(repo, "slot-7", "lane/fresh")
    for age in (0.0, 0.6, 99.0):
        write_lease(wt, holder_pid=dead_pid(), age_h=age)
        cat, reason, _ = aw.slot_state(wt)
        check(f"dead holder, {age}h old -> IN-USE", cat == aw.IN_USE, f"{cat}: {reason}")
    check("dead-holder reason names the pid", "holder pid" in reason and "gone" in reason, reason)


def acquire_args(branch, name="t"):
    return SimpleNamespace(tip=None, branch=branch, name=name, owner="lane-x", holder_pid=None)


def acquire(branch):
    """Run the real cmd_acquire; returns (slot name or None, output)."""
    out = io.StringIO()
    try:
        with patch.object(aw, "slot_has_live_session", return_value=False), \
                redirect_stdout(out), redirect_stderr(out):
            aw.cmd_acquire(acquire_args(branch))
    except SystemExit as e:
        return None, out.getvalue() + str(e.code)
    for line in out.getvalue().splitlines():
        if line.startswith("SLOT:"):
            return line.split()[1], out.getvalue()
    return None, out.getvalue()


def test_acquire_skips_a_leased_clean_landed_slot(repo):
    """BUG-wznn: the lane between commits has a clean tree, a landed HEAD, no
    process and a dead holder pid. Acquire must still leave its slot alone."""
    leased = add_slot(repo, "slot-0", "lane/working")
    write_lease(leased, owner="lane-a", task="work", holder_pid=dead_pid(), age_h=99.0)
    spare = add_slot(repo, "slot-1", "lane/spare")
    slot, text = acquire("lane/new")
    check("acquire takes the unleased slot", slot == "slot-1", text)
    check("leased slot keeps its branch",
          sh(leased, "git", "branch", "--show-current") == "lane/working", text)
    check("leased slot keeps its lease", (leased / aw.LEASE_NAME).exists(), text)
    check("new holder got its own lease", (spare / aw.LEASE_NAME).exists(), text)

    with patch.object(aw, "MAX_SLOTS", 2):
        slot, text = acquire("lane/third")
    check("with every slot leased the ring is full, not raided",
          slot is None and "POOL FULL" in text, text)
    check("full ring left both lanes in place",
          sh(leased, "git", "branch", "--show-current") == "lane/working"
          and sh(spare, "git", "branch", "--show-current") == "lane/new", text)


def test_acquire_takes_a_slot_after_release(repo):
    wt = add_slot(repo, "slot-0", "lane/done")
    write_lease(wt, owner="lane-a", task="work", holder_pid=dead_pid(), age_h=0.0)
    with patch.object(aw, "MAX_SLOTS", 1):
        slot, text = acquire("lane/blocked")
        check("leased only slot is not taken", slot is None and "POOL FULL" in text, text)
        with patch.object(aw, "slot_has_live_session", return_value=False), \
                redirect_stdout(io.StringIO()) as out:
            aw.cmd_release(SimpleNamespace(slot="slot-0"))
        check("release drops the lease", not (wt / aw.LEASE_NAME).exists(), out.getvalue())
        slot, text = acquire("lane/after-release")
    check("released slot is taken again", slot == "slot-0", text)
    check("released slot now holds the new branch",
          sh(wt, "git", "branch", "--show-current") == "lane/after-release", text)


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


def test_retire_preserves_real_dirty_tree(repo):
    wt = add_slot(repo, "slot-0", "lane/retirement")
    head = sh(wt, "git", "rev-parse", "HEAD")
    (wt / "f.txt").write_text("important modified source\n")
    (wt / "new file.txt").write_text("new source\n")
    handoff = b"Original handoff with exact details\n"
    (wt / "WORKTREE_HANDOFF.md").write_bytes(handoff)
    with patch.object(aw, "slot_has_live_session", return_value=False):
        aw.cmd_retire(SimpleNamespace(slot="slot-0", include=["new file.txt"]))
    ref = sh(repo, "git", "for-each-ref", "--format=%(refname)", "refs/heads/archive/worktrees")
    sha = sh(repo, "git", "rev-parse", ref)
    check("archive preserves modified source", sh(repo, "git", "show", ref + ":f.txt") == "important modified source")
    check("archive preserves exact handoff", sh(repo, "git", "show", ref + ":WORKTREE_HANDOFF.md") + "\n" == handoff.decode())
    check("archive preserves explicitly included path with space", sh(repo, "git", "show", ref + ":new file.txt") == "new source")
    check("original branch retained", sh(repo, "git", "rev-parse", "lane/retirement") == head)
    check("retirement ends clean", not sh(wt, "git", "status", "--porcelain"))
    check("retirement detaches to main", sh(wt, "git", "rev-parse", "HEAD") == sh(repo, "git", "rev-parse", "origin/main"))
    check("remote archive SHA matches", sh(repo, "git", "ls-remote", "origin", ref).split()[0] == sha)


def test_retire_refusals_and_failed_push(repo):
    wt = add_slot(repo, "slot-0", "lane/retire-failure")
    (wt / "f.txt").write_text("preserve me\n")
    (wt / "unknown.txt").write_text("not reviewed\n")
    args = SimpleNamespace(slot="slot-0", include=[])
    with patch.object(aw, "slot_has_live_session", return_value=False):
        try:
            aw.cmd_retire(args)
            check("unknown file blocks retirement", False)
        except SystemExit:
            check("unknown file blocks retirement", True)
        args.include = ["../escape"]
        try:
            aw.cmd_retire(args)
            check("include path traversal refused", False)
        except SystemExit:
            check("include path traversal refused", True)
        args.include = ["unknown.txt"]
        sh(repo, "git", "remote", "set-url", "--push", "origin", str(repo / "missing-remote"))
        before = sh(wt, "git", "status", "--porcelain")
        try:
            aw.cmd_retire(args)
            check("push failure stops retirement", False)
        except SystemExit:
            check("push failure stops retirement", True)
        check("failed push retains dirty tree", sh(wt, "git", "status", "--porcelain") == before and (wt / "f.txt").read_text() == "preserve me\n")
        check("failed push retains local archive", bool(sh(repo, "git", "for-each-ref", "--format=%(refname)", "refs/heads/archive/worktrees")))
    with patch.object(aw, "slot_has_live_session", return_value=True):
        try:
            aw.cmd_retire(args)
            check("live process blocks retirement", False)
        except SystemExit:
            check("live process blocks retirement", True)


def test_fixture_pruning_and_process_failure(repo):
    (repo / ".gitignore").write_text("*.manifold\n.claude/*\n")
    fixture = repo / "tests/fixtures/nested/keep.manifold"
    fixture.parent.mkdir(parents=True); fixture.write_text("fixture")
    hidden = repo / ".claude/worktrees-quarantine-slot-2/tests/fixtures/leak.manifold"
    hidden.parent.mkdir(parents=True); hidden.write_text("do not copy")
    wt = add_slot(repo, "slot-0", "lane/fixtures")
    aw.copy_missing_fixtures(wt)
    check("nested fixture copied", (wt / "tests/fixtures/nested/keep.manifold").exists())
    check("quarantine fixture excluded", not (wt / hidden.relative_to(repo)).exists())
    with patch.object(aw.subprocess, "run", return_value=SimpleNamespace(returncode=1, stdout="")):
        check("process scan error fails closed", aw.slot_has_live_session(wt))
    try:
        aw.refuse_if_branch_ref_exists("lane/fixtures", wt, aw.branch_holders())
        check("existing branch cannot be reset", False)
    except SystemExit:
        check("existing branch cannot be reset", True)


def test_pool_lock_excludes_all_lifecycle_commands(repo):
    holder = start_lock_holder(repo)
    try:
        commands = [
            (["acquire", "blocked", "lane/blocked"], "acquire"),
            (["list"], "list"),
            (["scrub"], "scrub"),
            (["release", "slot-0"], "release"),
            (["retire", "slot-0"], "retire"),
            (["remove", "slot-0"], "remove"),
        ]
        for argv, name in commands:
            result = run_module_subprocess(repo, argv)
            check(f"pool lock excludes {name}", result.returncode != 0 and
                  "worktree pool is busy" in result.stderr, result.stderr + result.stdout)
        check("blocked acquire does not create checkout",
              not (aw.POOL / "slot-0").exists(), str(list(aw.POOL.iterdir())))
    finally:
        holder.terminate()
        holder.wait(timeout=5)


def test_pool_lock_releases_after_failure_and_process_exit(repo):
    try:
        with aw.pool_lock():
            raise RuntimeError("synthetic command failure")
    except RuntimeError:
        pass
    with aw.pool_lock():
        check("pool lock releases after command failure", True)

    holder = start_lock_holder(repo)
    holder.terminate()
    holder.wait(timeout=5)
    result = run_module_subprocess(repo, ["list"])
    check("pool lock releases after process exit", result.returncode == 0,
          result.stderr + result.stdout)


# ---------------------------------------------------------------------- main

def test_retire_remote_mismatch_and_concurrent_edit(repo):
    wt = add_slot(repo, "slot-0", "lane/race")
    args = SimpleNamespace(slot="slot-0", include=[])
    original_git = aw.git
    for mode in ("mismatch", "edit"):
        (wt / "f.txt").write_text("original dirty source\n")
        def controlled_git(cwd, *args, **kwargs):
            result = original_git(cwd, *args, **kwargs)
            if args and args[0] == "ls-remote":
                if mode == "mismatch":
                    result.stdout = "0" * 40 + "\t" + args[-1] + "\n"
                else:
                    (wt / "f.txt").write_text("new concurrent edit\n")
            return result
        with patch.object(aw, "git", side_effect=controlled_git), patch.object(aw, "slot_has_live_session", return_value=False):
            try:
                aw.cmd_retire(args)
                check(mode + " blocks retirement", False)
            except SystemExit:
                check(mode + " blocks retirement", True)
        expected = "new concurrent edit\n" if mode == "edit" else "original dirty source\n"
        check(mode + " retains source", (wt / "f.txt").read_text() == expected)


def test_retire_deleted_and_literal_paths(repo):
    wt = add_slot(repo, "slot-0", "lane/rename")
    (wt / "f.txt").rename(wt / "renamed file.txt")
    (wt / "[literal].txt").write_text("literal filename\n")
    with patch.object(aw, "slot_has_live_session", return_value=False):
        aw.cmd_retire(SimpleNamespace(slot="slot-0", include=["renamed file.txt", "[literal].txt"]))
    ref = sh(repo, "git", "for-each-ref", "--format=%(refname)", "refs/heads/archive/worktrees")
    listing = sh(repo, "git", "ls-tree", "--name-only", ref).splitlines()
    check("archive preserves rename and literal path", "f.txt" not in listing and "renamed file.txt" in listing and "[literal].txt" in listing)
    check("renamed retirement ends clean", not sh(wt, "git", "status", "--porcelain"))


TESTS = [
    test_retire_remote_mismatch_and_concurrent_edit,
    test_retire_deleted_and_literal_paths,
    test_retire_preserves_real_dirty_tree,
    test_retire_refusals_and_failed_push,
    test_fixture_pruning_and_process_failure,
    test_clean_landed_no_lease_is_idle,
    test_clean_landed_old_lease_stays_in_use,
    test_clean_landed_live_lease_is_in_use,
    test_live_holder_old_lease_stays_in_use,
    test_dirty_is_never_reclaimable,
    test_unlanded_sole_holder_is_never_reclaimable,
    test_unlanded_duplicate_is_reclaimable,
    test_dead_holder_never_frees_a_lease,
    test_acquire_skips_a_leased_clean_landed_slot,
    test_acquire_takes_a_slot_after_release,
    test_pool_full_groups_each_slot_correctly,
    test_acquire_refuses_a_branch_held_elsewhere,
    test_acquire_allows_the_slot_that_already_holds_it,
    test_pool_lock_excludes_all_lifecycle_commands,
    test_pool_lock_releases_after_failure_and_process_exit,
]


def test_remove_requires_ignored_asset_backup(repo):
    wt = add_slot(repo, "slot-0", "lane/remove")
    asset = wt / ".claude/unique.txt"
    asset.parent.mkdir(exist_ok=True)
    asset.write_text("unique local asset\n")
    args = SimpleNamespace(slot="slot-0", recovery=None)
    with patch.object(aw, "slot_has_live_session", return_value=False):
        try:
            aw.cmd_remove(args)
            check("unique ignored asset blocks removal", False)
        except SystemExit:
            check("unique ignored asset blocks removal", wt.exists())
        (repo / ".claude/unique.txt").write_bytes(asset.read_bytes())
        aw.cmd_remove(args)
    check("verified duplicate checkout removed", not wt.exists())
    check("removed checkout branch retained", bool(sh(repo, "git", "rev-parse", "lane/remove")))


TESTS.append(test_remove_requires_ignored_asset_backup)


def fake_target(wt, name="gen_node_catalog-0f1c97c31eb2a77a", size=2 * 2**20):
    """A Cargo-tagged target holding one hashed Mach-O executable in deps/."""
    deps = wt / "target" / "debug" / "deps"
    deps.mkdir(parents=True)
    (wt / "target" / "CACHEDIR.TAG").write_text("Signature: 8a477f597d28d172789f06886806bc55\n")
    exe = deps / name
    exe.write_bytes(b"\xcf\xfa\xed\xfe" + b"\0" * (size - 4))
    exe.chmod(0o755)
    old = time.time() - 7200
    os.utime(exe, (old, old))
    return exe


def test_scrub_frees_an_idle_slot_over_its_cap(repo):
    """BUG-vnp8: `release slot-0` reported "removed 0 files (0.0G) from 49.0G"
    because the residue was all hashed executables. Over-cap idle slots must
    actually lose that cache."""
    wt = add_slot(repo, "slot-0", "lane/landed")
    exe = fake_target(wt)
    with patch.object(aw, "slot_cap_bytes", return_value=1), \
            patch.object(aw, "slot_has_live_session", return_value=False), \
            patch.object(aw, "target_live_status", return_value=False), \
            redirect_stdout(io.StringIO()) as out:
        aw.cmd_scrub(SimpleNamespace())
    check("over-cap idle slot loses its executables", not exe.exists(), out.getvalue())
    check("scrub reports the removed file", "removed 1 cache files" in out.getvalue(), out.getvalue())
    check("checkout untouched", (wt / "f.txt").read_text() == "base\n")


def test_reclaim_touches_only_landed_clean_idle_slots(repo):
    landed = add_slot(repo, "slot-0", "lane/landed")
    landed_exe = fake_target(landed)
    dirty = add_slot(repo, "slot-1", "lane/dirty")
    (dirty / "f.txt").write_text("uncommitted\n")
    dirty_exe = fake_target(dirty)
    unlanded = add_slot(repo, "slot-2", "lane/unlanded")
    (unlanded / "g.txt").write_text("x\n")
    sh(unlanded, "git", "add", "g.txt")
    sh(unlanded, "git", "commit", "-qm", "unlanded")
    unlanded_exe = fake_target(unlanded)
    leased = add_slot(repo, "slot-3", "lane/leased")
    write_lease(leased, holder_pid=os.getpid(), age_h=0.5)
    leased_exe = fake_target(leased)
    spare = add_slot(repo, "slot-4", "lane/spare")
    spare_exe = fake_target(spare)
    # Oldest build first: slot-0 is the LRU victim, slot-4 is newer.
    old = time.time() - 10800
    for path in (landed_exe, landed / "target", *(landed / "target").iterdir()):
        os.utime(path, (old, old))

    def free_space(_path):
        return 10**15 if not landed_exe.exists() else 0

    with patch.object(aw, "disk_free", side_effect=free_space), \
            patch.object(aw, "slot_has_live_session", return_value=False), \
            patch.object(aw, "target_live_status", return_value=False), \
            redirect_stdout(io.StringIO()) as out:
        aw.cmd_reclaim(SimpleNamespace(free_bytes=100 * 2**30))
    text = out.getvalue()
    check("landed idle slot cache reclaimed", not landed_exe.exists(), text)
    check("reclaim stops once the reserve is met", spare_exe.exists(), text)
    check("dirty slot untouched", dirty_exe.exists() and (dirty / "f.txt").read_text() == "uncommitted\n", text)
    check("unlanded slot untouched", unlanded_exe.exists(), text)
    check("leased slot untouched", leased_exe.exists(), text)
    check("reclaim names what it kept", "KEEP slot-1: dirty" in text and "KEEP slot-2: unlanded" in text, text)
    check("reclaim reports the freed slot", "RECLAIMED: removed 1" in text, text)

    with patch.object(aw, "disk_free", return_value=0), \
            patch.object(aw, "slot_has_live_session", return_value=False), \
            patch.object(aw, "target_live_status", return_value=False), \
            redirect_stdout(io.StringIO()):
        try:
            aw.cmd_reclaim(SimpleNamespace(free_bytes=100 * 2**30))
            check("reclaim exits nonzero when the reserve stays unmet", False)
        except SystemExit as e:
            check("reclaim exits nonzero when the reserve stays unmet", e.code == 3, repr(e.code))
    check("second pass still leaves pinned slots alone",
          dirty_exe.exists() and unlanded_exe.exists() and leased_exe.exists())


def test_reclaim_refuses_a_live_process(repo):
    wt = add_slot(repo, "slot-0", "lane/live")
    exe = fake_target(wt)
    with patch.object(aw, "disk_free", return_value=0), \
            patch.object(aw, "slot_has_live_session", return_value=False), \
            patch.object(aw, "target_live_status", return_value=True), \
            redirect_stdout(io.StringIO()) as out:
        try:
            aw.cmd_reclaim(SimpleNamespace(free_bytes=100 * 2**30))
        except SystemExit:
            pass
    check("live target keeps its cache", exe.exists(), out.getvalue())
    check("live refusal is reported", "live process" in out.getvalue(), out.getvalue())


def test_scrub_continues_past_a_victim_that_frees_nothing(repo):
    """One unmarked target freed nothing and ended the pass, so the warm caches
    behind it stayed on disk (2026-10-06: pool left at 117G of a 40G goal)."""
    empty = add_slot(repo, "slot-0", "lane/empty")
    (empty / "target" / "debug").mkdir(parents=True)
    (empty / "target" / "debug" / "stray").write_bytes(b"\0" * 2**20)
    old = time.time() - 3600
    for path in (empty / "target", empty / "target" / "debug"):
        os.utime(path, (old, old))
    warm = add_slot(repo, "slot-1", "lane/warm")
    exe = fake_target(warm)
    with patch.object(aw, "SCRUB_TO_GB", 0), \
            patch.object(aw, "slot_has_live_session", return_value=False), \
            patch.object(aw, "target_live_status", return_value=False), \
            redirect_stdout(io.StringIO()) as out:
        aw.cmd_scrub(SimpleNamespace())
    check("scrub reaches the cache behind a no-op victim", not exe.exists(), out.getvalue())


TESTS += [test_scrub_frees_an_idle_slot_over_its_cap,
          test_reclaim_touches_only_landed_clean_idle_slots,
          test_reclaim_refuses_a_live_process,
          test_scrub_continues_past_a_victim_that_frees_nothing]


def test_list_exposes_cache_budgets(repo):
    wt = add_slot(repo, "slot-0", "lane/list-cache")
    fake_target(wt)
    with patch.object(aw, "slot_cap_bytes", return_value=1), redirect_stdout(io.StringIO()) as out:
        aw.cmd_list(SimpleNamespace())
    text = out.getvalue()
    check("list exposes cap incremental age and reserve", all(word in text for word in
          ("/slot", "60m", "50G", "incremental", "OVER")), text)


TESTS.append(test_list_exposes_cache_budgets)


# ------------------------------------------------- idle Codex plugin brokers

cb = sys.modules["codex_brokers"]

# Stands in for the plugin's broker. Mode "answer" replies to broker/shutdown
# the way app-server-broker.mjs does (reply, unlink socket and pid file, exit);
# "silent" accepts and never replies; "linger" replies and keeps running;
# "nosocket" never listens. Saved under the broker script's name so the
# command-line identity check sees a broker.
FAKE_BROKER = textwrap.dedent("""
    import json, os, socket, sys, time
    mode, sock_path, pid_file = sys.argv[1], sys.argv[2], sys.argv[3]
    open(pid_file, "w").write(str(os.getpid()))
    if mode == "nosocket":
        print("READY", flush=True)
        time.sleep(60)
        sys.exit(0)
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(sock_path)
    server.listen(4)
    print("READY", flush=True)
    while True:
        conn, _ = server.accept()
        if mode == "silent":
            time.sleep(60)
        line = conn.makefile().readline()
        if line and json.loads(line).get("method") == "broker/shutdown":
            conn.sendall(b'{"id":1,"result":{}}\\n')
            conn.close()
            if mode == "linger":
                continue
            server.close()
            os.unlink(sock_path)
            os.unlink(pid_file)
            sys.exit(0)
        conn.close()
""")


class Brokers:
    """Fake plugin state root, Codex sessions dir, and broker processes whose
    cwd is a slot — the exact shape that pinned the live ring."""

    def __init__(self, repo):
        self.root = repo.parent / "plugin-state"
        self.sessions = repo.parent / "codex-sessions"
        self.sessions.mkdir()
        self.script = repo.parent / cb.BROKER_SCRIPT
        self.script.write_text(FAKE_BROKER)
        self.procs, self.dirs = [], []

    def patches(self):
        return (patch.object(cb, "state_roots", return_value=[self.root]),
                patch.object(cb, "CODEX_SESSIONS", self.sessions))

    def start(self, wt, jobs=(), mode="answer", rollouts=True):
        state = self.root / cb.state_dir_name(wt)
        state.mkdir(parents=True, exist_ok=True)
        # AF_UNIX paths cap at 104 bytes on macOS; temp dirs run long.
        session = Path(tempfile.mkdtemp(prefix="cxb-", dir="/tmp"))
        self.dirs.append(session)
        sock, pid_file, log = session / "broker.sock", session / "broker.pid", session / "broker.log"
        log.write_text("")
        proc = subprocess.Popen([sys.executable, str(self.script), mode, str(sock), str(pid_file)],
                                cwd=str(wt), stdout=subprocess.PIPE, text=True)
        self.procs.append(proc)
        if proc.stdout.readline().strip() != "READY":
            raise RuntimeError("fake broker failed to start")
        # The real broker is detached and reaped by launchd; reap ours so an
        # exited one never reads as alive.
        threading.Thread(target=proc.wait, daemon=True).start()
        self.write_broker(wt, {"endpoint": f"unix:{sock}", "pidFile": str(pid_file),
                               "logFile": str(log), "sessionDir": str(session), "pid": proc.pid})
        self.set_jobs(wt, jobs)
        # A real finished job always left a rollout; write a closed, quiet one
        # unless the test already wrote its own.
        for job in jobs if rollouts else ():
            thread = job.get("threadId")
            if thread and not any(self.sessions.glob(f"*/*/*/rollout-*-{thread}.jsonl")):
                self.rollout(thread, ["task_started", "task_complete"], age_s=3600)
        return proc, state, session

    def companion(self):
        """A live process under the plugin worker's script name."""
        script = self.script.with_name(cb.COMPANION_SCRIPT)
        script.write_text("import time\ntime.sleep(60)\n")
        proc = subprocess.Popen([sys.executable, str(script)])
        self.procs.append(proc)
        return proc

    def write_broker(self, wt, record):
        state = self.root / cb.state_dir_name(wt)
        state.mkdir(parents=True, exist_ok=True)
        (state / "broker.json").write_text(json.dumps(record))

    def set_jobs(self, wt, jobs):
        (self.root / cb.state_dir_name(wt) / "state.json").write_text(
            json.dumps({"version": 1, "jobs": list(jobs)}))

    def rollout(self, thread, events, age_s):
        """A rollout whose event_msg lines carry these turn markers, in order."""
        path = self.sessions / "2026" / "10" / "06" / f"rollout-2026-10-06T00-00-00-{thread}.jsonl"
        path.parent.mkdir(parents=True, exist_ok=True)
        filler = json.dumps({"type": "response_item", "payload": {"text": "x" * 70000}})
        lines = []
        for event in events:
            lines += [json.dumps({"timestamp": "2026-10-06T00:00:00.000Z", "type": "event_msg",
                                  "payload": {"type": event}}, separators=(",", ":")), filler]
        path.write_text("\n".join(lines) + "\n")
        stamp = time.time() - age_s
        os.utime(path, (stamp, stamp))

    def stop_all(self):
        for proc in self.procs:
            if proc.poll() is None:
                proc.kill()
                proc.wait()
        for path in self.dirs:
            shutil.rmtree(path, ignore_errors=True)


def completed_job(thread="0a1b-c2"):
    return {"id": "task-a", "status": "completed", "pid": None, "threadId": thread,
            "updatedAt": time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime())}


def wait_gone(proc, seconds=5.0):
    deadline = time.time() + seconds
    while proc.poll() is None and time.time() < deadline:
        time.sleep(0.05)
    return proc.poll() is not None


def test_state_dir_matches_the_plugin_layout(repo):
    """The plugin hashes the realpath of git's toplevel, which git has already
    resolved through symlinks, and slugs that resolved name."""
    real = repo.parent / "real parent" / "My Slot!"
    real.mkdir(parents=True)
    link = repo.parent / "link"
    link.symlink_to(real)
    import hashlib
    expected = "My-Slot-" + hashlib.sha256(str(real.resolve()).encode()).hexdigest()[:16]
    check("state dir name follows the plugin", cb.state_dir_name(link) == expected,
          cb.state_dir_name(link))


def test_idle_codex_broker_no_longer_pins_a_slot(repo):
    """The live-ring failure: every slot an Astra job ran in kept a broker whose
    cwd was the slot, so acquire skipped them all and the pool read full."""
    wt = add_slot(repo, "slot-0", "lane/done")
    brokers = Brokers(repo)
    try:
        brokers.rollout("0a1b-c2", ["task_started", "task_complete"], age_s=3600)
        proc, state, session = brokers.start(wt, jobs=[completed_job()])
        check("an idle broker reads as a live session", aw.slot_has_live_session(wt))
        roots, sessions = brokers.patches()
        out = io.StringIO()
        with roots, sessions, patch.object(aw, "MAX_SLOTS", 1), \
                redirect_stdout(out), redirect_stderr(out):
            try:
                aw.cmd_acquire(acquire_args("lane/next"))
            except SystemExit as e:
                out.write(f"exit {e.code}")
        text = out.getvalue()
        check("acquire stops the idle broker", "STOPPED broker" in text, text)
        check("acquire reuses the slot", "SLOT:     slot-0" in text, text)
        check("broker process exited", wait_gone(proc), text)
        check("broker log is retained", session.joinpath("broker.log").exists(), text)
        check("broker record and socket files cleared",
              not (state / "broker.json").exists() and not (session / "broker.sock").exists(), text)
        check("job history kept", (state / "state.json").exists())
    finally:
        brokers.stop_all()


def test_busy_codex_broker_is_left_alone(repo):
    wt = add_slot(repo, "slot-0", "lane/busy")
    brokers = Brokers(repo)
    try:
        worker = brokers.companion()
        # The plugin stamps updatedAt on phase changes only, so a long turn
        # carries its start time: a day-old stamp with a live worker is busy.
        day_old = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime(time.time() - 86400))
        running = dict(completed_job(), status="running", pid=worker.pid, updatedAt=day_old)
        proc, _, _ = brokers.start(wt, jobs=[running])
        roots, sessions = brokers.patches()
        with roots, sessions:
            lines = cb.stop_idle(wt)
            check("running job keeps its broker at any age",
                  proc.poll() is None and "is running" in lines[0], lines)

            brokers.set_jobs(wt, [dict(running, pid=os.getpid())])
            lines = cb.stop_idle(wt)
            check("a stale running job whose pid now belongs to something else does not",
                  "is running" not in lines[0], lines)
            proc, _, _ = brokers.start(wt)

            brokers.set_jobs(wt, [dict(completed_job(), status="queued")])
            lines = cb.stop_idle(wt)
            check("queued job keeps its broker", proc.poll() is None and "is queued" in lines[0], lines)

            # The plugin marks a job completed on a mid-turn message; Codex can
            # then go quiet for minutes inside a build. Only the markers tell.
            brokers.set_jobs(wt, [completed_job()])
            brokers.rollout("0a1b-c2", ["task_started", "task_complete", "task_started"], age_s=1800)
            lines = cb.stop_idle(wt)
            check("open turn keeps its broker through a long quiet build",
                  proc.poll() is None and "turn still open" in lines[0], lines)

            brokers.set_jobs(wt, [dict(completed_job(), status="cancelled")])
            brokers.rollout("0a1b-c2", ["task_started"], age_s=7200)
            lines = cb.stop_idle(wt)
            check("a killed turn, open but silent for hours, does not",
                  lines == [f"STOPPED broker pid {proc.pid}"] and wait_gone(proc), lines)
            proc, _, _ = brokers.start(wt, jobs=[completed_job()])

            brokers.rollout("0a1b-c2", [], age_s=3600)
            lines = cb.stop_idle(wt)
            check("a rollout with no turn markers keeps its broker, loudly",
                  proc.poll() is None and "no turn markers" in lines[0], lines)

            brokers.rollout("0a1b-c2", ["task_started", "turn_aborted"], age_s=5)
            lines = cb.stop_idle(wt)
            check("closed but freshly written rollout keeps its broker",
                  proc.poll() is None and "written in the last" in lines[0], lines)

            brokers.rollout("0a1b-c2", ["task_started", "task_complete"], age_s=3600)
            lines = cb.stop_idle(wt)
            check("closed quiet turn lets the broker stop",
                  lines == [f"STOPPED broker pid {proc.pid}"] and wait_gone(proc), lines)

            proc, _, _ = brokers.start(wt, jobs=[completed_job("missing-rollout")], rollouts=False)
            lines = cb.stop_idle(wt)
            check("a recent completed job without a rollout keeps its broker",
                  proc.poll() is None and "no rollout found" in lines[0], lines)

            brokers.set_jobs(wt, [completed_job(None)])
            lines = cb.stop_idle(wt)
            check("a job that never reached Codex does not pin its broker",
                  lines == [f"STOPPED broker pid {proc.pid}"] and wait_gone(proc), lines)
    finally:
        brokers.stop_all()


def test_leased_slot_keeps_its_broker_until_release(repo):
    wt = add_slot(repo, "slot-0", "lane/leased")
    write_lease(wt, holder_pid=os.getpid())
    brokers = Brokers(repo)
    try:
        proc, _, _ = brokers.start(wt, jobs=[completed_job()])
        roots, sessions = brokers.patches()
        with roots, sessions, patch.object(aw, "target_live_status", return_value=False):
            with redirect_stdout(io.StringIO()) as out:
                aw.cmd_scrub(SimpleNamespace())
            check("scrub leaves a leased slot's broker", proc.poll() is None, out.getvalue())
            with redirect_stdout(io.StringIO()) as out:
                aw.cmd_release(SimpleNamespace(slot="slot-0"))
            check("release stops it", "STOPPED broker" in out.getvalue() and wait_gone(proc), out.getvalue())
    finally:
        brokers.stop_all()


def test_odd_brokers_are_never_signalled(repo):
    wt = add_slot(repo, "slot-0", "lane/odd")
    brokers = Brokers(repo)
    try:
        roots, sessions = brokers.patches()
        with roots, sessions, patch.object(cb, "EXIT_WAIT_S", 0.3):
            silent, state, _ = brokers.start(wt, jobs=[completed_job()], mode="silent")
            lines = cb.stop_idle(wt)
            check("a broker that never answers is kept, not killed",
                  silent.poll() is None and "did not answer" in lines[0], lines)
            check("its record is kept", (state / "broker.json").exists())

            deaf, _, _ = brokers.start(wt, jobs=[completed_job()], mode="nosocket")
            lines = cb.stop_idle(wt)
            check("a running broker without its socket is kept, not killed",
                  deaf.poll() is None and "socket gone" in lines[0], lines)

            deaf.kill()
            wait_gone(deaf)
            lines = cb.stop_idle(wt)
            check("a dead broker's record is cleared",
                  "CLEARED stale" in lines[0] and not (state / "broker.json").exists(), lines)

            # After a reboot the recorded pid can be any process, here this one.
            brokers.write_broker(wt, {"endpoint": "unix:/tmp/cxb-missing/broker.sock", "pid": os.getpid()})
            lines = cb.stop_idle(wt)
            check("a reused pid is not mistaken for the broker",
                  "CLEARED stale" in lines[0] and not (state / "broker.json").exists(), lines)

            linger, _, _ = brokers.start(wt, jobs=[completed_job()], mode="linger")
            lines = cb.stop_idle(wt)
            check("an acknowledged broker that stays up is kept",
                  linger.poll() is None and "still running" in lines[0], lines)
            lines = cb.stop_idle(wt)
            check("and is not asked again while it exits", "earlier, still exiting" in lines[0], lines)
    finally:
        brokers.stop_all()


def test_malformed_plugin_state_never_breaks_the_ring(repo):
    wt = add_slot(repo, "slot-0", "lane/malformed")
    brokers = Brokers(repo)
    roots, sessions = brokers.patches()
    with roots, sessions:
        record = {"endpoint": "unix:/tmp/cxb-missing/broker.sock", "pid": None}
        for jobs in ({"a": 1}, "jobs", [None]):
            brokers.write_broker(wt, record)
            (brokers.root / cb.state_dir_name(wt) / "state.json").write_text(json.dumps({"jobs": jobs}))
            lines = cb.stop_idle(wt)
            check(f"jobs {jobs!r} fails closed", "unreadable" in lines[0], lines)
        brokers.set_jobs(wt, [])
        for bad in ([1, 2], {"endpoint": 7, "pid": 1}, {"endpoint": "unix:/x", "pidFile": 3}):
            brokers.write_broker(wt, bad)
            lines = cb.stop_idle(wt)
            check(f"broker {bad!r} fails closed", "unreadable broker.json" in lines[0], lines)
    with patch.object(aw, "stop_idle_codex_brokers", side_effect=RuntimeError("boom")), \
            redirect_stdout(io.StringIO()) as out:
        aw.stop_idle_brokers([wt])
    check("a helper crash is reported, not raised", "helper failed" in out.getvalue(), out.getvalue())


def test_turn_markers_are_found_across_chunk_boundaries(repo):
    """The rollout is read backwards in chunks; a marker split by a chunk edge
    must still count, and an earlier marker must never shadow a later one."""
    path = repo.parent / "rollout.jsonl"
    opened, closed = cb.TURN_MARKERS[0], cb.TURN_MARKERS[1]
    bad = []
    with patch.object(cb, "TAIL_CHUNK", 64):
        for shift in range(0, 200, 7):
            path.write_bytes(closed + b"x" * 300 + opened + b"y" * shift)
            if cb.last_turn_marker(path) != "open":
                bad.append(("open", shift))
            path.write_bytes(opened + b"x" * 300 + closed + b"y" * shift)
            if cb.last_turn_marker(path) != "closed":
                bad.append(("closed", shift))
        path.write_bytes(b"z" * 500)
        none = cb.last_turn_marker(path)
    check("markers found at every chunk offset", not bad, bad)
    check("no marker reads as none", none is None, none)


TESTS += [test_state_dir_matches_the_plugin_layout,
          test_turn_markers_are_found_across_chunk_boundaries,
          test_idle_codex_broker_no_longer_pins_a_slot,
          test_busy_codex_broker_is_left_alone,
          test_leased_slot_keeps_its_broker_until_release,
          test_odd_brokers_are_never_signalled,
          test_malformed_plugin_state_never_breaks_the_ring]


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
