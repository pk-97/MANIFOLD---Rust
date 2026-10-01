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
import textwrap
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
    return exe


def test_scrub_frees_an_idle_slot_over_its_cap(repo):
    """BUG-vnp8: `release slot-0` reported "removed 0 files (0.0G) from 49.0G"
    because the residue was all hashed executables. Over-cap idle slots must
    actually lose that cache."""
    wt = add_slot(repo, "slot-0", "lane/landed")
    exe = fake_target(wt)
    with patch.object(aw, "TARGET_CAP_GB", 0), \
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
    old = time.time() - 3600
    for path in (landed / "target", *(landed / "target").iterdir()):
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
    check("reclaim reports the freed slot", "RECLAIMED slot-0" in text, text)

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


TESTS += [test_scrub_frees_an_idle_slot_over_its_cap,
          test_reclaim_touches_only_landed_clean_idle_slots,
          test_reclaim_refuses_a_live_process]


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
