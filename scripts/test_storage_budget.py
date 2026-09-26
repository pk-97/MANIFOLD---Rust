#!/usr/bin/env python3
"""Focused tests for the bounded Cargo target inventory and cleanup manifest."""

import os
import fcntl
import tempfile
from pathlib import Path
from contextlib import contextmanager
from types import SimpleNamespace
from unittest.mock import patch

import storage_budget as sb


PASS, FAIL = [], []


@contextmanager
def fixture_directory():
    """Dispose only of generated test fixtures, one file at a time."""
    root = Path(tempfile.mkdtemp(prefix="storage-budget-")).resolve()
    try:
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
            low = sb.check_build(target, repo, free_bytes=99 * sb.GIB)
            good = sb.check_build(target, repo, free_bytes=101 * sb.GIB)
        check("unknown target override rejected", not unknown and "canonical" in unknown.reason, unknown.reason)
        check("low free space rejected", not low and "reserve" in low.reason, low.reason)
        check("canonical target with reserve accepted", bool(good), good.reason)
        with patch.object(sb.subprocess, "run", return_value=SimpleNamespace(returncode=1, stdout="", stderr="registry unavailable")):
            report = sb.inventory_targets(repo, tmp_root=root / "no-tmp")
        check("inventory reports Git registry failure", report.errors and "registry unavailable" in report.errors[0], str(report.errors))


def test_manifest_dry_run_apply_and_identity():
    with fixture_directory() as raw:
        target = Path(raw).resolve() / "target"
        marker(target)
        cache = target / "debug" / "deps"
        cache.mkdir(parents=True)
        generated = cache / "libgenerated-abcdef.rlib"
        generated.write_bytes(b"generated")
        unknown = target / "debug" / "manifold-proof.bin"
        unknown.write_bytes(b"proof")
        binary = target / "manifold"
        binary.write_bytes(b"keep")
        link = cache / "linked"
        link.symlink_to(unknown)
        for subtree in ("incremental", ".fingerprint", "build", "examples"):
            subtree_root = target / "debug" / subtree
            subtree_root.mkdir(parents=True)
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
        cache = target / "release" / "build"
        cache.mkdir(parents=True)
        file = cache / "build-script"
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
        with patch.object(sb.subprocess, "run", return_value=SimpleNamespace(returncode=0, stdout=lsof)):
            check("checkout cwd is live", sb.target_live_status(target) is True)
        lsof = "p123\nf3\nn" + str(file) + "\n"
        with patch.object(sb.subprocess, "run", return_value=SimpleNamespace(returncode=0, stdout=lsof)):
            check("open target descriptor is live", sb.target_live_status(target) is True)
        lsof = "p123\nfcwd\n" + str(root / "outside") + "\n"
        with patch.object(sb.subprocess, "run", return_value=SimpleNamespace(returncode=0, stdout=lsof)):
            check("unrelated process is idle", sb.target_live_status(target) is False)

        cargo_lock = target / ".cargo-lock"
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


for test in (test_inventory_and_symlink_boundary, test_build_admission,
             test_manifest_dry_run_apply_and_identity, test_live_and_uninspectable_refused,
             test_lsof_and_cargo_lock_safety, test_ancestor_symlink_refused_by_fd_traversal):
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
