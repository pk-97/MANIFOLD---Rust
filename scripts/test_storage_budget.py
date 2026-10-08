#!/usr/bin/env python3
"""Focused tests for the bounded Cargo target inventory and cleanup manifest."""

import os
import fcntl
import tempfile
import time
import subprocess
import sys
from pathlib import Path
from contextlib import contextmanager
from types import SimpleNamespace
from unittest.mock import patch

import storage_budget as sb


PASS, FAIL = [], []
REAL_REGISTRY = sb.registered_worktrees


@contextmanager
def fixture_directory():
    """Dispose only of generated test fixtures, one file at a time."""
    root = Path(tempfile.mkdtemp(prefix="storage-budget-")).resolve()
    pool = root / ".claude" / "worktrees"
    pool.mkdir(parents=True)
    (pool / ".agent-worktree.lock").touch()
    def registry(_repo):
        return tuple(dict.fromkeys([root, *(p.parent.parent for p in root.rglob(".rustc_info.json"))]))
    try:
        with patch.object(sb, "registered_worktrees", side_effect=registry):
            yield str(root)
    finally:
        for directory, dirs, files in os.walk(root, topdown=False, followlinks=False):
            for name in files:
                (Path(directory) / name).unlink()
            for name in dirs:
                path = Path(directory) / name
                if path.is_symlink():
                    path.unlink()  # The fixture link itself, never its target.
                else:
                    path.rmdir()  # Empty-only.
        root.rmdir()


def check(name, condition, detail=""):
    (PASS if condition else FAIL).append(name if condition else (name, detail))


def lsof_result(output):
    """Synthetic lsof records carry device/inode fields just like real files."""
    lines = []
    for line in output.splitlines():
        lines.append(line)
        if line.startswith("n"):
            try:
                st = Path(line[1:]).stat()
                device, inode = st.st_dev, st.st_ino
            except OSError:
                device, inode = 0, 0
            lines.extend((f"D{device:x}", f"i{inode}"))
    return SimpleNamespace(returncode=0, stdout="\n".join(lines), stderr="")


def marker(target):
    target.mkdir(parents=True, exist_ok=True)
    (target / ".rustc_info.json").write_text("{}\n")


def test_inventory_and_symlink_boundary():
    with fixture_directory() as raw:
        root = Path(raw).resolve()
        repo = root / "repo"
        main_target = repo / "target"
        marker(main_target)
        (main_target / "debug").mkdir()
        (main_target / "debug" / "inside.bin").write_bytes(b"inside")
        outside = root / "outside.bin"
        outside.write_bytes(b"must not count")
        (main_target / "debug" / "escape").symlink_to(outside)
        temporary = root / "tmp" / "manifold-probe" / "target"
        marker(temporary)
        (temporary / "release").mkdir()
        (temporary / "release" / "tmp.bin").write_bytes(b"tmp")
        direct = root / "tmp" / "manifold-uv1-target"
        marker(direct)
        (direct / "debug").mkdir()
        (direct / "debug" / "direct.bin").write_bytes(b"direct")
        shared = main_target / "shared.bin"
        shared.write_bytes(b"shared")
        os.link(shared, direct / "debug" / "shared-hardlink.bin")
        with patch.object(sb, "registered_worktrees", return_value=(repo,)):
            records = sb.inventory_targets(repo, tmp_root=root / "tmp").targets
        paths = {record.path for record in records}
        check("inventory includes main target", main_target in paths, str(paths))
        check("inventory includes tagged temporary target", temporary in paths, str(paths))
        check("inventory includes direct temporary target", direct in paths, str(paths))
        main = next(record for record in records if record.path == main_target)
        expected = sum(path.stat().st_blocks * sb.ALLOCATED_BLOCK
                       for path in (main_target / ".rustc_info.json", main_target / "debug" / "inside.bin", shared))
        check("inventory does not follow symlink", main.size_bytes == expected, main.size_bytes)
        naive_total = sum(record.size_bytes for record in records)
        naive_apparent = sum(
            path.stat().st_blocks * sb.ALLOCATED_BLOCK
            for path in (main_target / ".rustc_info.json", main_target / "debug" / "inside.bin", shared,
                         direct / ".rustc_info.json", direct / "debug" / "direct.bin",
                         direct / "debug" / "shared-hardlink.bin",
                         temporary / ".rustc_info.json", temporary / "release" / "tmp.bin"))
        check("inventory de-duplicates hard-linked blocks", shared.stat().st_blocks == 0 or naive_total < naive_apparent, naive_total)


def test_build_admission():
    with fixture_directory() as raw:
        root = Path(raw).resolve()
        repo = root / "repo"
        target = repo / "target"
        target.mkdir(parents=True)
        with patch.object(sb, "registered_worktrees", return_value=(repo,)):
            unknown = sb.check_build(root / "other" / "target", repo, free_bytes=200 * sb.GIB)
            low = sb.check_build(target, repo, free_bytes=49 * sb.GIB)
            good = sb.check_build(target, repo, free_bytes=51 * sb.GIB)
        check("unknown target override rejected", not unknown and "canonical" in unknown.reason, unknown.reason)
        check("low free space rejected", not low and "reserve" in low.reason, low.reason)
        check("canonical target with reserve accepted", bool(good), good.reason)
        with patch.object(sb, "registered_worktrees", REAL_REGISTRY), \
                patch.object(sb.subprocess, "run", return_value=SimpleNamespace(returncode=1, stdout="", stderr="registry unavailable")):
            report = sb.inventory_targets(repo, tmp_root=root / "no-tmp")
        check("inventory reports Git registry failure", report.errors and "registry unavailable" in report.errors[0], str(report.errors))


def test_manifest_dry_run_apply_and_identity():
    with fixture_directory() as raw:
        target = Path(raw).resolve() / "target"
        marker(target)
        cache = target / "debug" / "build" / "other-abcdef"
        cache.mkdir(parents=True)
        generated = cache / "output"
        generated.write_bytes(b"generated")
        unknown = target / "debug" / "manifold-proof.bin"
        unknown.write_bytes(b"proof")
        binary = target / "manifold"
        binary.write_bytes(b"keep")
        link = cache / "linked"
        link.symlink_to(unknown)
        for subtree in ("incremental", ".fingerprint", "build", "examples"):
            subtree_root = target / "debug" / subtree
            subtree_root.mkdir(parents=True, exist_ok=True)
            (subtree_root / "proof.png").write_bytes(b"unknown")
            (subtree_root / "notes.txt").write_bytes(b"unknown")
        (target / "debug" / "incremental" / "crate-abcdef").mkdir(parents=True)
        (target / "debug" / "incremental" / "crate-abcdef" / "s-hgw9xb7-2p31hz8-0dnkx7m").mkdir(parents=True)
        (target / "debug" / "incremental" / "crate-abcdef" / "s-hgw9xb7-2p31hz8-0dnkx7m" / "query-cache.bin").write_bytes(b"generated")
        (target / "debug" / ".fingerprint" / "crate-abcdef").mkdir(parents=True)
        (target / "debug" / ".fingerprint" / "crate-abcdef" / "dep-lib-crate").write_bytes(b"generated")
        (target / "debug" / "build" / "crate-abcdef").mkdir(parents=True)
        (target / "debug" / "build" / "crate-abcdef" / "output").write_bytes(b"generated")
        (target / "debug" / "examples" / "example-demo-abcdef").write_bytes(b"generated")
        (target / "debug" / "build" / "crate-abcdef" / "build-script-build").write_bytes(b"binary")
        plan = sb.plan_cache_cleanup(target)
        check("manifest excludes unknown files", not any(e.path.name == "proof.png" for e in plan.entries), str(plan.entries))
        check("manifest contains recognized Cargo files", generated in [e.path for e in plan.entries], str(plan.entries))
        check("manifest preserves arbitrary build binary", not any(e.path.name == "build-script-build" for e in plan.entries), str(plan.entries))
        before = generated.read_bytes()
        dry = sb.apply_cache_cleanup(plan, dry_run=True, process_check=lambda _: False)
        check("dry run reports without mutation", dry[1] == len(plan.entries) and generated.read_bytes() == before, str(dry))
        applied = sb.apply_cache_cleanup(plan, dry_run=False, process_check=lambda _: False)
        check("apply removes generated file", applied[1] == len(plan.entries) and not generated.exists(), str(applied))
        check("unknown files remain", binary.exists() and unknown.exists() and
              all((target / "debug" / subtree / filename).exists()
                  for subtree in ("incremental", ".fingerprint", "build", "examples")
                  for filename in ("proof.png", "notes.txt")), "unknown path removed")
        check("cache directories remain", cache.is_dir(), "cache directory removed")

        generated.write_bytes(b"new contents")
        changed = sb.plan_cache_cleanup(target)
        generated.write_bytes(b"changed after plan")
        result = sb.apply_cache_cleanup(changed, dry_run=False, process_check=lambda _: False)
        check("changed identity is preserved", generated.exists() and result[2], str(result))


def test_live_and_uninspectable_refused():
    with fixture_directory() as raw:
        target = Path(raw).resolve() / "target"
        marker(target)
        cache = target / "release" / "build" / "crate-abcdef"
        cache.mkdir(parents=True)
        file = cache / "output"
        file.write_bytes(b"build")
        plan = sb.plan_cache_cleanup(target)
        live = sb.apply_cache_cleanup(plan, dry_run=False, process_check=lambda _: True)
        unavailable = sb.apply_cache_cleanup(plan, dry_run=False, process_check=lambda _: None)
        check("live target refused", live[1] == 0 and "live" in live[2][0], str(live))
        check("uninspectable target refused", unavailable[1] == 0 and "unavailable" in unavailable[2][0], str(unavailable))
        check("refused cleanup leaves file", file.exists(), "file removed")


def test_lsof_and_cargo_lock_safety():
    with fixture_directory() as raw:
        root = Path(raw).resolve()
        target = root / "checkout" / "target"
        marker(target)
        cache = target / "debug" / "deps"
        cache.mkdir(parents=True)
        file = cache / "libsafe-abcdef.rlib"
        file.write_bytes(b"safe")
        plan = sb.plan_cache_cleanup(target)
        lsof = "p123\nfcwd\nn" + str(root / "checkout") + "\n"
        with patch.object(sb.subprocess, "run", return_value=lsof_result(lsof)):
            check("checkout cwd is live", sb.target_live_status(target) is True)
        lsof = "p123\nf3\nn" + str(file) + "\n"
        with patch.object(sb.subprocess, "run", return_value=lsof_result(lsof)):
            check("open target descriptor is live", sb.target_live_status(target) is True)
        lsof = "p123\nfcwd\nn" + str(root / "outside") + "\n"
        with patch.object(sb.subprocess, "run", return_value=lsof_result(lsof)):
            check("unrelated process is idle", sb.target_live_status(target) is False)

        cargo_lock = target / "debug" / ".cargo-lock"
        cargo_lock.write_bytes(b"lock")
        lock_handle = cargo_lock.open("rb")
        fcntl.flock(lock_handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        try:
            blocked = sb.apply_cache_cleanup(plan, dry_run=False, process_check=lambda _: False)
            check("held Cargo lock refuses cleanup", blocked[1] == 0 and "Cargo lock" in blocked[2][0], str(blocked))
            check("Cargo lock remains", cargo_lock.exists(), "lock removed")
        finally:
            fcntl.flock(lock_handle.fileno(), fcntl.LOCK_UN)
            lock_handle.close()


def test_ancestor_symlink_refused_by_fd_traversal():
    with fixture_directory() as raw:
        root = Path(raw).resolve()
        real = root / "real" / "target"
        marker(real)
        cache = real / "debug" / "deps"
        cache.mkdir(parents=True)
        generated = cache / "libsafe-abcdef.rlib"
        generated.write_bytes(b"safe")
        alias = root / "alias"
        alias.symlink_to(root / "real", target_is_directory=True)
        plan = sb.plan_cache_cleanup(alias / "target")
        result = sb.apply_cache_cleanup(plan, dry_run=False, process_check=lambda _: False)
        check("ancestor symlink is refused", result[1] == 0 and result[2], str(result))
        check("ancestor symlink target remains", generated.exists(), "file removed through symlink")


def test_real_cargo_names():
    # Names copied from a live slot target, 2026-09-30.
    rec = sb._recognized_cargo_file
    check("codegen-unit object recognized", rec("deps", (
        "manifold_playback-45cffb150ab52a7e.3ffzzz259qhxqz222i6agvff6.13qn3pm.rcgu.o",)))
    check("named codegen-unit object recognized", rec("deps", (
        "graph_tool-2642b0d0949f57f3.graph_tool.ad5d32f312c1c1c1-cgu.15.rcgu.o",)))
    check("fingerprint hash file recognized", rec(".fingerprint", ("deflate64-07af2637e01f5bd0", "lib-deflate64")))
    check("build-script run fingerprint recognized", rec(".fingerprint", (
        "coremidi-sys-bd69ec384c8c8575", "run-build-script-build-script-build.json")))
    check("hashed executable kept by name alone", not rec("deps", ("structured_modifier_echo-0f1c97c31eb2a77a",)))
    check("build-script output still kept", not rec("build", (
        "libmimalloc-sys-38e0194b3fec4450", "out", "077ae3504b1c7768-static.o")))
    check("object without a hash kept", not rec("deps", ("notes.3ffzzz.rcgu.o",)))


def test_hashed_executables():
    """The 2026-10-01 residue: a scrubbed slot kept 35 GiB of extensionless test
    and bin executables because the manifest only knew names with a suffix."""
    with fixture_directory() as raw:
        target = Path(raw).resolve() / "target"
        marker(target)
        deps = target / "debug" / "deps"
        deps.mkdir(parents=True)
        examples = target / "debug" / "examples"
        examples.mkdir(parents=True)
        macho = b"\xcf\xfa\xed\xfe" + b"\0" * 60
        exe = deps / "gen_node_catalog-0f1c97c31eb2a77a"
        exe.write_bytes(macho)
        exe.chmod(0o755)
        example = examples / "blob_demo-45cffb150ab52a7e"
        example.write_bytes(b"\xca\xfe\xba\xbe" + b"\0" * 60)
        example.chmod(0o755)
        no_exec_bit = deps / "quiet-45cffb150ab52a7e"
        no_exec_bit.write_bytes(macho)
        no_exec_bit.chmod(0o644)
        script = deps / "helper-45cffb150ab52a7e"
        script.write_bytes(b"#!/bin/sh\necho hi\n")
        script.chmod(0o755)
        short_hash = deps / "manifold-abcdef"
        short_hash.write_bytes(macho)
        short_hash.chmod(0o755)
        unhashed = examples / "blob_demo"
        unhashed.write_bytes(macho)
        unhashed.chmod(0o755)
        link = deps / "linked-45cffb150ab52a7e"
        link.symlink_to(exe)
        plan = sb.plan_cache_cleanup(target)
        planned = {entry.path for entry in plan.entries}
        check("hashed Mach-O test executable planned", exe in planned, str(planned))
        check("hashed fat-binary example planned", example in planned, str(planned))
        check("Mach-O without exec bit kept", no_exec_bit not in planned, str(planned))
        check("executable shell script kept", script not in planned, str(planned))
        check("short hash kept", short_hash not in planned, str(planned))
        check("unhashed example kept", unhashed not in planned, str(planned))
        check("symlink kept", link not in planned, str(planned))
        applied = sb.apply_cache_cleanup(plan, dry_run=False, process_check=lambda _: False)
        check("executables retained without launch exclusion", applied[1] == 0 and exe.exists() and example.exists() and applied[2], str(applied))
        check("kept files remain", all(p.exists() for p in (no_exec_bit, script, short_hash, unhashed)) and link.is_symlink(), "kept file removed")
        exe.write_bytes(macho)
        exe.chmod(0o755)
        plan = sb.plan_cache_cleanup(target)
        exe.chmod(0o644)
        result = sb.apply_cache_cleanup(plan, dry_run=False, process_check=lambda _: False)
        check("mode change after plan is preserved", exe.exists() and result[2], str(result))


def stale_session(target, name="s-aaa-bbb-ccc", age=7200):
    marker(target)
    session = target / "debug" / "incremental" / "crate-abc123" / name
    session.mkdir(parents=True)
    cache = session / "query-cache.bin"
    cache.write_bytes(b"cache" * 1024)
    old = time.time() - age
    os.utime(cache, (old, old))
    os.utime(session, (old, old))
    return session, cache


def test_bounded_reclaim_order_and_preservation():
    with fixture_directory() as raw:
        root = Path(raw)
        target = root / "target"
        old, old_file = stale_session(target, age=10800)
        newer, new_file = stale_session(target, "s-ddd-eee-fff")
        fresh, fresh_file = stale_session(target, "s-ggg-hhh-iii", age=30)
        mixed, mixed_file = stale_session(target, "s-jjj-kkk-lll")
        (mixed / "notes.txt").write_text("user report")
        os.utime(mixed, (time.time() - 7200,) * 2)
        protected = [target / "fixtures" / "asset.bin",
                     target / "landing-logs" / "gate.log",
                     target / "my-report" / "result.rlib",
                     target / "debug" / "build" / "crate-abcdef" / "out" / "asset.o"]
        for path in protected:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("keep")
        deps = target / "debug" / "deps" / "libold-abcdef.rlib"
        deps.parent.mkdir()
        deps.write_bytes(b"artifact")
        os.utime(deps, (time.time() - 14400,) * 2)
        free = lambda _: sb.MAINTENANCE_GOAL_BYTES if not old.exists() else 0
        result = sb.maintain_caches([target], root, process_check=lambda *a, **k: False,
                                   free_check=free)
        check("oldest incremental session reclaimed before older deps", not old.exists() and deps.exists(), result)
        check("reserve stops reclamation", newer.exists() and fresh.exists(), result)
        check("fixtures logs reports and build outputs preserved", all(p.exists() for p in protected), result)
        check("unknown session contents protect entire directory", mixed_file.exists(), result)
        # Cap maintenance runs even with ample disk space and stops before deps.
        size = sb.target_size(target)
        amount = new_file.stat().st_blocks * sb.ALLOCATED_BLOCK
        result = sb.maintain_caches([target], root, cap_bytes=size - amount,
                                   process_check=lambda *a, **k: False,
                                   free_check=lambda _: 100 * sb.GIB)
        check("cap prunes incremental first despite healthy reserve", not newer.exists() and deps.exists(), result)
        check("fresh sessions survive cap pressure", fresh_file.exists(), result)
        result = sb.maintain_caches([target], root, process_check=lambda *a, **k: False,
                                   free_check=lambda _: 0)
        check("deps retained even under exhausted reserve", deps.exists() and result[2], result)
        check("protected files survive exhausted reserve", fresh_file.exists() and mixed_file.exists()
              and all(p.exists() for p in protected), result)


def test_bounded_reclaim_liveness_and_lock():
    with fixture_directory() as raw:
        root = Path(raw)
        target = root / "target"
        session, cache = stale_session(target)
        for live in (True, None):
            result = sb.maintain_caches([target], root, process_check=lambda *a, **k: live,
                                       free_check=lambda _: 0)
            check(f"bounded reclaim protects liveness={live}", cache.exists() and result[1] == 0 and result[2], result)
        lock = target / "debug" / ".cargo-lock"
        lock.touch()
        with lock.open("rb") as handle:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = sb.maintain_caches([target], root, process_check=lambda *a, **k: False,
                                       free_check=lambda _: 0)
            check("bounded reclaim respects held cargo lock", cache.exists() and result[2], result)
        checkout = target.parent
        lsof = f"p123\nfcwd\nn{checkout}\n"
        with patch.object(sb.subprocess, "run", return_value=lsof_result(lsof)):
            check("same-slot cwd permits maintenance", sb.target_live_status(target, include_checkout=False) is False)
            check("foreign-slot cwd blocks maintenance", sb.target_live_status(target) is True)
        lsof += f"f4\nn{cache}\n"
        with patch.object(sb.subprocess, "run", return_value=lsof_result(lsof)):
            check("same-slot open cache still blocks maintenance", sb.target_live_status(target, include_checkout=False) is True)
        lsof = f"p{os.getpid()}\nf3\nn{lock}\np123\nfcwd\nn/outside\n"
        with patch.object(sb.subprocess, "run", return_value=lsof_result(lsof)):
            check("maintenance's own Cargo lock does not mark target live", sb.target_live_status(target) is False)
        lsof = f"p{os.getpid()}\nf4\nn{cache}\n"
        with patch.object(sb.subprocess, "run", return_value=lsof_result(lsof)):
            check("caller's own open cache is protected", sb.target_live_status(target) is True)
        for response in (SimpleNamespace(returncode=0, stdout="malformed"),
                         SimpleNamespace(returncode=0, stdout=lsof, stderr="incomplete process scan")):
            with patch.object(sb.subprocess, "run", return_value=response):
                check("uncertain process snapshot fails closed", sb.target_live_status(target) is None)


def test_admission_reclaims_same_slot_and_idle_only():
    with fixture_directory() as raw:
        repo = Path(raw) / "main"
        pool = repo / ".claude" / "worktrees"
        own, idle, leased = (pool / f"slot-{i}" for i in range(3))
        own_session, cache = stale_session(own / "target")
        (pool / ".agent-worktree.lock").touch()
        _, idle_cache = stale_session(idle / "target", age=9000)
        _, leased_cache = stale_session(leased / "target", age=10000)
        _, main_cache = stale_session(repo / "target", age=11000)
        (own / ".worktree-lease.json").write_text("{}")
        (leased / ".worktree-lease.json").write_text("{}")
        free = lambda _: 100 * sb.GIB if not cache.exists() else 0
        with patch.object(sb, "registered_worktrees", return_value=(repo, own, idle, leased)), \
                patch.object(sb, "idle_slot", side_effect=lambda p: p == idle), \
                patch.object(sb, "process_snapshot") as snapshot, \
                patch.object(sb, "disk_free", side_effect=free):
            from unittest.mock import Mock
            live = Mock(return_value=False)
            snapshot.return_value = live
            admitted = sb.check_build(own / "target", own)
        check("admission reclaims owning leased slot and idle slot", admitted.ok and not cache.exists() and not idle_cache.exists(), admitted)
        check("admission never reclaims main or foreign leased slot", main_cache.exists() and leased_cache.exists())
        check("only admitting slot ignores checkout cwd", live.call_args_list[1].kwargs == {"include_checkout": False}
              and live.call_args_list[0].kwargs == {}, live.call_args_list)
        with patch.object(sb, "registered_worktrees", return_value=(repo, own, idle, leased)), \
                patch.object(sb, "idle_slot", return_value=False), \
                patch.object(sb, "process_snapshot", return_value=lambda *a, **k: False), \
                patch.object(sb, "disk_free", return_value=0):
            refused = sb.check_build(own / "target", own, reserve_bytes=1)
        check("reserve still refuses without safe reclaim and cannot be lowered",
              not refused.ok and refused.reserve_bytes == 50 * sb.GIB, refused)


def test_cap_configuration():
    with patch.dict(os.environ, {}, clear=True):
        check("default cap is 160 GiB", sb.slot_cap_bytes() == 160 * sb.GIB)
    with patch.dict(os.environ, {"MANIFOLD_SLOT_TARGET_CAP_GIB": "31"}):
        check("slot cap configurable", sb.slot_cap_bytes() == 31 * sb.GIB)
    for value in ("0", "-1", "nan", "inf", "bad"):
        with patch.dict(os.environ, {"MANIFOLD_SLOT_TARGET_CAP_GIB": value}):
            try:
                sb.slot_cap_bytes()
                check(f"bad cap {value} rejected", False)
            except ValueError:
                check(f"bad cap {value} rejected", True)


def test_same_slot_open_session_and_log():
    with fixture_directory() as raw:
        root = Path(raw)
        target = root / "target"
        busy, busy_file = stale_session(target, age=10000)
        idle, idle_file = stale_session(target, "s-ddd-eee-fff")
        log = target / "landing-logs" / "gate.log"
        log.parent.mkdir()
        log.write_text("keep this open log")
        lsof = f"p123\nfcwd\nn{root}\nf3\nn{busy_file}\nf4\nn{log}\n"
        with patch.object(sb.subprocess, "run", return_value=lsof_result(lsof)):
            snapshot = sb.process_snapshot()
        result = sb.maintain_caches([target], root, same_slot=target,
                                   process_check=snapshot, free_check=lambda _: 0)
        check("open session directory is never removed", busy_file.exists() and busy.is_dir(), result)
        check("open landing log does not pin unrelated stale session", log.exists() and not idle.exists(), result)


def test_admission_cap_with_healthy_reserve_and_fresh_refusal():
    with fixture_directory() as raw:
        repo = Path(raw) / "main"
        own = repo / ".claude" / "worktrees" / "slot-0"
        session, cache = stale_session(own / "target")
        (own.parent / ".agent-worktree.lock").touch()
        with patch.object(sb, "registered_worktrees", return_value=(repo, own)), \
                patch.object(sb, "process_snapshot", return_value=lambda *a, **k: False), \
                patch.object(sb, "slot_cap_bytes", return_value=1), \
                patch.object(sb, "disk_free", return_value=100 * sb.GIB):
            result = sb.check_build(own / "target", own)
        check("admission enforces slot budget with sufficient free space", result.ok and not session.exists(), result)
        session, cache = stale_session(own / "target", age=30)
        with patch.object(sb, "registered_worktrees", return_value=(repo, own)), \
                patch.object(sb, "process_snapshot", return_value=lambda *a, **k: False), \
                patch.object(sb, "disk_free", return_value=49 * sb.GIB):
            result = sb.check_build(own / "target", own)
        check("admission refuses rather than deleting a fresh session", not result.ok and cache.exists(), result)


def test_admission_pool_reservation_safety():
    with fixture_directory() as raw:
        pool = Path(raw)
        with sb.admission_pool_lock(pool) as locked:
            check("missing pool lock skips automatic cleanup", not locked)
        lock = pool / ".agent-worktree.lock"
        check("admission never creates pool lock", not lock.exists())
        lock.touch()
        with lock.open("rb") as holder:
            fcntl.flock(holder, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with sb.admission_pool_lock(pool) as locked:
                check("pool reservation prevents admission cleanup", not locked)
        with sb.admission_pool_lock(pool) as locked:
            check("released pool reservation permits maintenance", locked)


def test_build_starting_during_cleanup():
    """A subprocess follows Cargo's flock protocol at the inventory barrier."""
    with fixture_directory() as raw:
        root = Path(raw)
        target = root / "target"
        _, cache = stale_session(target)
        lock = target / "debug" / ".cargo-lock"
        check("race starts without a Cargo lock file", not lock.exists())
        original_plan = sb.plan_cache_cleanup
        builders = []
        program = """
import fcntl, os, sys
fd = os.open(sys.argv[1], os.O_RDWR | os.O_CREAT, 0o666)
try:
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    print('UNEXPECTED', flush=True)
except BlockingIOError:
    print('BLOCKED', flush=True)
    fcntl.flock(fd, fcntl.LOCK_EX)
print('ACQUIRED', flush=True)
os.close(fd)
"""
        def inventory_after_builder_start(path, profiles=None):
            child = subprocess.Popen([sys.executable, "-c", program, str(lock)],
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            builders.append(child)
            import select
            ready, _, _ = select.select([child.stdout], [], [], 5)
            assert ready, "builder did not reach lock barrier"
            check("build starting before inventory blocks on created Cargo lock",
                  child.stdout.readline().strip() == "BLOCKED")
            return original_plan(path, profiles)
        try:
            with patch.object(sb, "plan_cache_cleanup", side_effect=inventory_after_builder_start):
                result = sb.maintain_caches([target], root, same_slot=target,
                                           process_check=lambda *a, **k: False,
                                           free_check=lambda _: 0)
            check("rustc session removed while exclusion is held", not cache.exists(), result)
            output, errors = builders[0].communicate(timeout=5)
            check("waiting build acquires Cargo lock after cleanup", output.strip() == "ACQUIRED"
                  and builders[0].returncode == 0, (output, errors))
            check("Cargo lock inode remains for future builds", lock.is_file())
        finally:
            for child in builders:
                if child.poll() is None:
                    child.kill()
                child.communicate()


def test_busy_profile_does_not_block_other_profiles():
    with fixture_directory() as raw:
        root = Path(raw)
        target = root / "target"
        _, busy = stale_session(target)
        metadata = target / "release" / "build" / "crate-abcdef" / "output"
        metadata.parent.mkdir(parents=True)
        metadata.write_text("metadata")
        os.utime(metadata, (time.time() - 7200,) * 2)
        lock = target / "debug" / ".cargo-lock"
        with lock.open("w+") as holder:
            fcntl.flock(holder, fcntl.LOCK_EX)
            with patch.object(sb, "plan_cache_cleanup", wraps=sb.plan_cache_cleanup) as inventory:
                result = sb.maintain_caches([target], root, process_check=lambda _: False,
                                           free_check=lambda _: 0)
            check("busy profile excluded before inventory", inventory.call_args.args[1] == [target / "release"])
            check("busy profile preserved while unlocked profile reclaimed",
                  busy.exists() and not metadata.exists() and "Cargo lock" in str(result[2]), result)


def test_binary_opened_after_enumeration():
    for same_slot in (False, True):
        with fixture_directory() as raw:
            root = Path(raw)
            target = root / "target"
            _, cache = stale_session(target)
            artifacts = []
            for subtree in ("deps", "examples"):
                directory = target / "debug" / subtree
                directory.mkdir()
                for name in ("runner-0123456789abcdef", "libfoo-abcdef.dylib"):
                    artifact = directory / name
                    artifact.write_bytes(b"\xcf\xfa\xed\xfe" + bytes(60))
                    artifact.chmod(0o755)
                    os.utime(artifact, (time.time() - 7200,) * 2)
                    artifacts.append(artifact)
            if same_slot:
                (root / ".worktree-lease.json").write_text('{"holder_pid": %d}' % os.getpid())
            original_plan = sb.plan_cache_cleanup
            opened = []
            def open_after_inventory(path, profiles=None):
                plan = original_plan(path, profiles)
                opened.extend(p.open("rb") for p in artifacts)
                return plan
            try:
                with patch.object(sb, "plan_cache_cleanup", side_effect=open_after_inventory):
                    result = sb.maintain_caches([target], root,
                                               same_slot=target if same_slot else None,
                                               process_check=lambda *a, **k: False,
                                               free_check=lambda _: 0)
                check(f"late-open artifacts survive same_slot={same_slot}",
                      all(p.exists() for p in artifacts) and result[2], result)
                check(f"rustc-only state remains reclaimable same_slot={same_slot}", not cache.exists(), result)
            finally:
                for handle in opened:
                    handle.close()


def test_hard_link_liveness_identity():
    with fixture_directory() as raw:
        root = Path(raw)
        target = root / "checkout" / "target"
        session, cache = stale_session(target)
        alias = root / "outside-alias"
        os.link(cache, alias)
        with alias.open("rb") as opened:
            st = os.fstat(opened.fileno())
            # The descriptor remains open even if its old name no longer resolves.
            alias.unlink()
            output = f"p{os.getpid()}\nf3\nD{st.st_dev:x}\ni{st.st_ino}\nn{alias}\n"
            with patch.object(sb.subprocess, "run", return_value=SimpleNamespace(
                    returncode=0, stdout=output, stderr="")):
                snapshot = sb.process_snapshot()
            check("hard-link descriptor pins checkout by device and inode", snapshot(target) is True)
            check("hard-link descriptor pins same-slot session by identity",
                  snapshot(session, include_checkout=False) is True)
            result = sb.maintain_caches([target], root, same_slot=target,
                                       process_check=snapshot, free_check=lambda _: 0)
            check("hard-link held cache is never removed", cache.exists() and result[1] == 0, result)


def test_unregistered_target_executor_confinement():
    with fixture_directory() as raw:
        root = Path(raw)
        rogue = root / ".claude" / "worktrees" / "slot-99"
        target = rogue / "target"
        _, cache = stale_session(target)
        (rogue / ".git").write_text("gitdir: nowhere")
        plan = sb.plan_cache_cleanup(target)
        with patch.object(sb, "registered_worktrees", return_value=(root,)):
            for result in (
                    sb.maintain_caches([target], root, free_check=lambda _: 0,
                                       process_check=lambda _: False),
                    sb.apply_cache_cleanup(plan, dry_run=False, repo=root,
                                           process_check=lambda _: False)):
                check("shared executor refuses unregistered Cargo-tagged checkout",
                      cache.exists() and result[1] == 0 and "registered worktree" in str(result[2]), result)
        check("refused checkout gets no Cargo lock writes", not (target / "debug" / ".cargo-lock").exists())


def test_executor_holds_reservation_for_validation_and_unlink():
    with fixture_directory() as raw:
        root = Path(raw)
        target = root / "target"
        _, cache = stale_session(target)
        lock = root / ".claude" / "worktrees" / ".agent-worktree.lock"
        original_validate, original_unlink = sb.canonical_target, sb._unlink_entry
        def assert_reserved():
            with lock.open("rb") as contender:
                try:
                    fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    return
                raise AssertionError("executor lost pool reservation")
        def validate(*args):
            assert_reserved()
            return original_validate(*args)
        def unlink(*args):
            assert_reserved()
            return original_unlink(*args)
        with patch.object(sb, "canonical_target", side_effect=validate), \
                patch.object(sb, "_unlink_entry", side_effect=unlink):
            result = sb.maintain_caches([target], root, process_check=lambda _: False,
                                       free_check=lambda _: 0)
        check("registry validation and deletion hold shared reservation", not cache.exists(), result)
        with lock.open("rb") as holder:
            fcntl.flock(holder, fcntl.LOCK_EX | fcntl.LOCK_NB)
            _, cache = stale_session(target)
            result = sb.maintain_caches([target], root, process_check=lambda _: False,
                                       free_check=lambda _: 0)
            check("busy reservation refuses direct maintenance", cache.exists() and result[2], result)


for test in (test_inventory_and_symlink_boundary, test_build_admission,
             test_manifest_dry_run_apply_and_identity, test_live_and_uninspectable_refused,
             test_lsof_and_cargo_lock_safety, test_ancestor_symlink_refused_by_fd_traversal,
             test_real_cargo_names, test_hashed_executables,
             test_bounded_reclaim_order_and_preservation, test_bounded_reclaim_liveness_and_lock,
             test_admission_reclaims_same_slot_and_idle_only, test_cap_configuration,
             test_same_slot_open_session_and_log,
             test_admission_cap_with_healthy_reserve_and_fresh_refusal,
             test_admission_pool_reservation_safety,
             test_build_starting_during_cleanup,
             test_busy_profile_does_not_block_other_profiles,
             test_binary_opened_after_enumeration,
             test_hard_link_liveness_identity,
             test_unregistered_target_executor_confinement,
             test_executor_holds_reservation_for_validation_and_unlink):
    try:
        test()
    except Exception as error:
        FAIL.append((test.__name__, repr(error)))

for result in PASS:
    print("PASS:", result)
for result in FAIL:
    print("FAIL:", result)
print(f"{len(PASS)} passed, {len(FAIL)} failed")
raise SystemExit(1 if FAIL else 0)
