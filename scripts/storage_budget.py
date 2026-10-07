#!/usr/bin/env python3
"""Bounded Cargo target inventory, build admission and cache cleanup.

Cleanup uses an exact Cargo-file manifest; only empty incremental session
directories may also be removed. Unknown files, links and proof captures stay.
Admission prunes caches older than an hour in idle slots and the admitting
slot, holding Cargo locks and failing closed on unavailable process inspection.
MANIFOLD_SLOT_TARGET_CAP_GIB configures the per-slot budget (default 25 GiB).
Budgets are best effort: protected data can exceed them; the 50 GiB reserve
still refuses a build when safe reclamation cannot meet it.
"""

from __future__ import annotations

import argparse
import dataclasses
import errno
import fcntl
import os
import re
import shutil
import stat as stat_module
import subprocess
import sys
import time
from contextlib import ExitStack, contextmanager
from pathlib import Path
from typing import Callable, Iterable, Optional


GIB = 2 ** 30
# Two concurrent landing gates (~20 GiB of build cache each) plus headroom.
MAINTENANCE_GOAL_BYTES = 50 * GIB
DEFAULT_SLOT_CAP_BYTES = 25 * GIB
STALE_CACHE_SECONDS = 60 * 60
DEFAULT_TMP_ROOT = Path("/private/tmp")
PROFILE_NAMES = ("debug", "release")
CARGO_MARKERS = (".rustc_info.json", "CACHEDIR.TAG")
CACHE_SUBTREES = frozenset(("incremental", "deps", ".fingerprint", "build", "examples"))
ALLOCATED_BLOCK = 512
O_NOFOLLOW = getattr(os, "O_NOFOLLOW", 0)
O_DIRECTORY = getattr(os, "O_DIRECTORY", 0)
KNOWN_ARTIFACT_SUFFIXES = frozenset((
    ".a", ".d", ".dll", ".dylib", ".lib", ".pdb", ".rmeta", ".rlib", ".so",
))
KNOWN_FINGERPRINT_NAMES = frozenset(("invoked.timestamp",))
KNOWN_BUILD_NAMES = frozenset(("invoked.timestamp", "output", "root-output", "stderr"))
KNOWN_INCREMENTAL_NAMES = frozenset(("dep-graph.bin", "query-cache.bin", "work-products.bin"))
# Mach-O (64-bit, fat) and ELF headers: a linked test or bin executable, which
# Cargo writes extensionless as <target>-<16 hex> in deps/ and examples/.
NATIVE_EXECUTABLE_MAGICS = frozenset((
    b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xca\xfe\xba\xbe", b"\x7fELF",
))


class StorageSafetyError(RuntimeError):
    """A target cannot be inspected or safely changed."""


@dataclasses.dataclass(frozen=True)
class TargetRecord:
    path: Path
    size_bytes: int
    source: str
    cargo_tagged: bool
    reason: str = ""
    errors: tuple[str, ...] = ()


@dataclasses.dataclass(frozen=True)
class Inventory:
    targets: tuple[TargetRecord, ...]
    total_bytes: int
    maintenance_goal_bytes: int = MAINTENANCE_GOAL_BYTES
    errors: tuple[str, ...] = ()

    @property
    def below_maintenance_goal(self) -> bool:
        return self.total_bytes < self.maintenance_goal_bytes


@dataclasses.dataclass(frozen=True)
class BuildCheck:
    ok: bool
    target: Path
    free_bytes: int
    reserve_bytes: int = MAINTENANCE_GOAL_BYTES
    reason: str = ""
    # True only for a reserve shortfall: the one refusal a cache reclaim can lift.
    reclaimable: bool = False

    def __bool__(self) -> bool:
        return self.ok


@dataclasses.dataclass(frozen=True)
class CacheEntry:
    path: Path
    size_bytes: int
    identity: tuple[int, int, int, int, int, int]


@dataclasses.dataclass(frozen=True)
class CleanupPlan:
    target: Path
    entries: tuple[CacheEntry, ...]
    bytes_total: int
    refusals: tuple[str, ...] = ()


def _absolute(path: Path) -> Path:
    return Path(os.path.abspath(os.fspath(path)))


def _contains_symlink(path: Path, stop: Path) -> bool:
    """Return true when an existing path component is a symlink."""
    path = _absolute(path)
    stop = _absolute(stop)
    try:
        relative = path.relative_to(stop)
    except ValueError:
        return True
    current = stop
    for part in relative.parts:
        current /= part
        if current.is_symlink():
            return True
    return False


def _regular_file(path: Path) -> bool:
    try:
        return path.is_file() and not path.is_symlink()
    except OSError:
        return False


def is_cargo_target(path: Path) -> bool:
    """Recognise a target only when Cargo left a non-link marker in it."""
    path = _absolute(path)
    if not path.is_dir() or path.is_symlink():
        return False
    return any(_regular_file(path / marker) for marker in CARGO_MARKERS)


def _safe_size(path: Path, seen: Optional[set[tuple[int, int]]] = None,
               errors: Optional[list[str]] = None) -> int:
    """Count allocated blocks without links, escapes or hard-link double count."""
    seen = set() if seen is None else seen
    errors = [] if errors is None else errors
    path = _absolute(path)
    if path.is_symlink():
        return 0
    if _regular_file(path):
        try:
            stat = path.lstat()
            key = (stat.st_dev, stat.st_ino)
            if key in seen:
                return 0
            seen.add(key)
            return stat.st_blocks * ALLOCATED_BLOCK
        except OSError as error:
            errors.append(f"cannot inspect {path}: {error}")
            return 0
    if not path.is_dir():
        return 0
    total = 0
    try:
        with os.scandir(path) as children:
            for child in children:
                child_path = Path(child.path)
                if child.is_symlink():
                    continue
                if child.is_file(follow_symlinks=False):
                    try:
                        stat = child.stat(follow_symlinks=False)
                        key = (stat.st_dev, stat.st_ino)
                        if key in seen:
                            continue
                        seen.add(key)
                        total += stat.st_blocks * ALLOCATED_BLOCK
                    except OSError as error:
                        errors.append(f"cannot inspect {child_path}: {error}")
                        continue
                elif child.is_dir(follow_symlinks=False):
                    total += _safe_size(child_path, seen, errors)
    except OSError as error:
        errors.append(f"cannot inspect {path}: {error}")
        return total
    return total


def _worktree_registry(repo: Path) -> tuple[tuple[Path, ...], Optional[str]]:
    """Read Git's registry and retain the failure reason for inventory reports."""
    repo = _absolute(repo)
    try:
        result = subprocess.run(
            ["git", "-C", str(repo), "worktree", "list", "--porcelain"],
            capture_output=True, text=True, check=False, timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return (), f"Git worktree inspection failed for {repo}: {error}"
    if result.returncode != 0:
        detail = result.stderr.strip() or f"exit {result.returncode}"
        return (), f"Git worktree inspection failed for {repo}: {detail}"
    paths: list[Path] = []
    for line in result.stdout.splitlines():
        if line.startswith("worktree "):
            candidate = _absolute(Path(line[9:]))
            if candidate not in paths and not _contains_symlink(candidate, candidate.parent):
                paths.append(candidate)
    if not paths:
        return (), f"Git worktree inspection returned no registered worktrees for {repo}"
    return tuple(paths), None


_LAST_REGISTRY_ERROR: Optional[str] = None


def registered_worktrees(repo: Path) -> tuple[Path, ...]:
    """Read Git's worktree registry, retaining only safe absolute paths."""
    global _LAST_REGISTRY_ERROR
    paths, error = _worktree_registry(repo)
    _LAST_REGISTRY_ERROR = error
    return paths


def canonical_target(target_dir: Path, repo: Path) -> tuple[bool, Path, str]:
    """Validate that ``target_dir`` is the target of a registered worktree."""
    target = _absolute(target_dir)
    roots = registered_worktrees(repo)
    if not roots:
        return False, target, "unable to inspect Git worktree registry"
    for root in roots:
        expected = root / "target"
        if target == expected:
            if _contains_symlink(root, root.parent) or target.is_symlink():
                return False, target, "target or worktree path is a symlink"
            return True, target, "canonical target"
    return False, target, "target directory is not the canonical target of a registered worktree"


def _record(path: Path, source: str, seen: set[tuple[int, int]]) -> TargetRecord:
    errors: list[str] = []
    try:
        path.lstat()
    except FileNotFoundError:
        return TargetRecord(path, 0, source, False, "missing")
    except OSError as error:
        message = f"cannot inspect {path}: {error}"
        return TargetRecord(path, 0, source, False, message, (message,))
    tagged = is_cargo_target(path)
    if path.is_symlink():
        return TargetRecord(path, 0, source, False, "symlink ignored")
    if not path.exists():
        return TargetRecord(path, 0, source, False, "missing")
    if not tagged:
        return TargetRecord(path, _safe_size(path, seen, errors), source, False,
                            "Cargo marker missing", tuple(errors))
    return TargetRecord(path, _safe_size(path, seen, errors), source, True, "",
                        tuple(errors))


def inventory_targets(repo: Path, pool_dir: Optional[Path] = None,
                      tmp_root: Path = DEFAULT_TMP_ROOT) -> Inventory:
    """Inventory main, registered slot and Cargo-tagged temporary targets.

    This function performs no writes and never follows a symlink.  ``tmp_root``
    is injectable for isolated tests; production callers use ``/private/tmp``.
    """
    repo = _absolute(repo)
    global _LAST_REGISTRY_ERROR
    _LAST_REGISTRY_ERROR = None
    roots = list(registered_worktrees(repo))
    inventory_errors: list[str] = [_LAST_REGISTRY_ERROR] if _LAST_REGISTRY_ERROR else []
    if pool_dir is not None:
        pool = _absolute(pool_dir)
        if pool.is_dir() and not pool.is_symlink():
            try:
                pool_children = sorted(pool.iterdir())
            except OSError as error:
                inventory_errors.append(f"cannot inspect {pool}: {error}")
                pool_children = []
            for child in pool_children:
                if not child.name.startswith("slot-"):
                    continue
                try:
                    is_directory = child.is_dir()
                    is_link = child.is_symlink()
                except OSError as error:
                    inventory_errors.append(f"cannot inspect {child}: {error}")
                    continue
                if is_directory and not is_link and child not in roots:
                    roots.append(child)
    records: list[TargetRecord] = []
    inode_seen: set[tuple[int, int]] = set()
    seen: set[Path] = set()
    for root in roots:
        target = root / "target"
        if target in seen:
            continue
        seen.add(target)
        records.append(_record(target, "worktree" if root != repo else "main", inode_seen))

    tmp_root = _absolute(tmp_root)
    if tmp_root.is_dir() and not tmp_root.is_symlink():
        try:
            candidates = sorted(tmp_root.iterdir())
        except OSError as error:
            inventory_errors.append(f"cannot inspect {tmp_root}: {error}")
            candidates = []
        for candidate in candidates:
            if not candidate.name.startswith("manifold-"):
                continue
            try:
                is_directory = candidate.is_dir()
                is_link = candidate.is_symlink()
            except OSError as error:
                inventory_errors.append(f"cannot inspect {candidate}: {error}")
                continue
            if not is_directory or is_link:
                continue
            # Cargo also accepts an explicit target directory directly named
            # manifold-*-target; older probes used manifold-*/target.  Record
            # both forms, de-duplicating exact paths and hard-linked files.
            candidates_for_entry = [candidate]
            nested = candidate / "target"
            if nested != candidate:
                candidates_for_entry.append(nested)
            for target in candidates_for_entry:
                if target in seen or not is_cargo_target(target):
                    continue
                seen.add(target)
                records.append(_record(target, "temporary", inode_seen))
    errors = tuple(inventory_errors) + tuple(error for record in records for error in record.errors)
    return Inventory(tuple(records), sum(record.size_bytes for record in records),
                     errors=errors)


def disk_free(path: Path) -> int:
    return shutil.disk_usage(_absolute(path)).free


def check_build(target_dir: Path, repo: Path, free_bytes: Optional[int] = None,
                reserve_bytes: int = MAINTENANCE_GOAL_BYTES) -> BuildCheck:
    """Maintain slot caches before admission. An injected free count is read-only."""
    reserve_bytes = max(reserve_bytes, MAINTENANCE_GOAL_BYTES)
    valid, target, reason = canonical_target(target_dir, repo)
    if not valid:
        return BuildCheck(False, target, 0 if free_bytes is None else free_bytes,
                          reserve_bytes, "REFUSED: " + reason)
    available = disk_free(target.parent if target.parent.exists() else Path.cwd()) if free_bytes is None else free_bytes
    if free_bytes is None:
        try:
            cap = slot_cap_bytes()
            roots = registered_worktrees(repo)
            slots = [root for root in roots[1:] if is_slot(root)]
            own = target.parent if target.parent in slots else None
            removed, files, failures = 0, 0, ()
            if slots:
                # Coordinate with acquire/release: eligibility cannot change
                # underneath this pass. Never create pool files from admission.
                with admission_pool_lock(slots[0].parent) as locked:
                    if locked:
                        eligible = [root / "target" for root in slots
                                    if root == own or idle_slot(root)]
                        removed, files, failures = maintain_caches(
                            eligible, target.parent, reserve_bytes, cap,
                            same_slot=own / "target" if own else None)
                    else:
                        failures = ("cache maintenance skipped: pool lock busy or unavailable",)
            if files:
                print(f"STORAGE: removed {files} stale cache files ({removed / GIB:.1f} GiB)",
                      file=sys.stderr)
            for failure in failures:
                print(f"STORAGE: {failure}", file=sys.stderr)
            available = disk_free(target.parent)
        except (OSError, ValueError) as error:
            return BuildCheck(False, target, available, reserve_bytes,
                              f"REFUSED: cache maintenance unavailable: {error}")
    if available < reserve_bytes:
        return BuildCheck(False, target, available, reserve_bytes,
                          f"REFUSED: only {available / GIB:.1f} GiB free; reserve is {reserve_bytes / GIB:.0f} GiB",
                          reclaimable=True)
    return BuildCheck(True, target, available, reserve_bytes, "build target and free-space reserve are valid")


def slot_cap_bytes() -> int:
    value = float(os.environ.get("MANIFOLD_SLOT_TARGET_CAP_GIB", str(DEFAULT_SLOT_CAP_BYTES / GIB)))
    if not 0 < value < float("inf"):
        raise ValueError("MANIFOLD_SLOT_TARGET_CAP_GIB must be finite and positive")
    return int(value * GIB)


@contextmanager
def admission_pool_lock(pool: Path):
    """Use the ring's existing reservation lock without following links."""
    with ExitStack() as stack:
        try:
            directory = _open_directory(pool)
            stack.callback(os.close, directory)
            fd = os.open(".agent-worktree.lock", os.O_RDONLY | O_NOFOLLOW, dir_fd=directory)
            stack.callback(os.close, fd)
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            yield False
            return
        yield True


def is_slot(root: Path) -> bool:
    return (bool(re.fullmatch(r"slot-[0-9]+", root.name))
            and root.parent.name == "worktrees" and root.parent.parent.name == ".claude"
            and not root.is_symlink())


def idle_slot(root: Path) -> bool:
    """A foreign slot must be clean, landed and lease-free; never infer lease expiry."""
    if (root / ".worktree-lease.json").exists():
        return False
    try:
        status = subprocess.run(["git", "-C", str(root), "status", "--porcelain"],
                                capture_output=True, text=True, timeout=10)
        landed = subprocess.run(["git", "-C", str(root), "merge-base", "--is-ancestor",
                                 "HEAD", "origin/main"], capture_output=True, timeout=10)
        return status.returncode == 0 and not status.stdout.strip() and landed.returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def target_size(target: Path) -> int:
    if target.is_symlink() or not target.is_dir():
        return 0
    result = subprocess.run(["du", "-sk", str(target)], capture_output=True, text=True)
    if result.returncode:
        raise OSError(f"cannot measure {target}: {result.stderr.strip()}")
    return int(result.stdout.split()[0]) * 1024


def incremental_size(target: Path) -> int:
    if target.is_symlink() or not target.is_dir():
        return 0
    return sum(target_size(profile / "incremental") for profile in target.iterdir()
               if _profile_dir(profile) and not profile.is_symlink())


def _profile_dir(path: Path) -> bool:
    return path.name in PROFILE_NAMES or path.name.startswith(("debug-", "release-"))


def _cache_path(target: Path, path: Path) -> bool:
    if _contains_symlink(path, target):
        return False
    try:
        relative = path.relative_to(target)
    except ValueError:
        return False
    parts = relative.parts
    if len(parts) < 3 or not _profile_dir(target / parts[0]) or parts[1] not in CACHE_SUBTREES:
        return False
    if any(part in ("", ".", "..") for part in parts):
        return False
    return _recognized_cargo_file(parts[1], parts[2:], path)


_HASHED_DIR = re.compile(r"^[A-Za-z0-9_.-]+-[0-9a-f]{6,}$")
_HASHED_ARTIFACT = re.compile(r"^[A-Za-z0-9_.-]+-[0-9a-f]{6,}(?:\.[A-Za-z0-9]+)?$")
# Covers the hash file (lib-foo), its JSON twin and the dep-info file, for
# compiled units and build-script runs.
_FINGERPRINT_METADATA = re.compile(
    r"^(?:dep-)?(?:lib|bin|test|example|build-script|run-build-script)-"
    r"[A-Za-z0-9_.-]+(?:\.json)?$")
# split-debuginfo = "unpacked" leaves one object per codegen unit beside each
# artifact: <crate>-<16 hex>.<cgu name>[.<id>].rcgu.o
_RCGU_OBJECT = re.compile(r"^[A-Za-z0-9_-]+-[0-9a-f]{16}(?:\.[A-Za-z0-9_-]+)+\.rcgu\.o$")
# Linked executables carry the metadata hash and no extension. Every change in
# feature unification mints a new hash, so a slot accumulates dozens of ~55 MB
# copies per test target; they were the whole residue of a scrubbed slot.
_HASHED_EXECUTABLE = re.compile(r"^[A-Za-z0-9_-]+-[0-9a-f]{16}$")


def _native_executable(path: Path) -> bool:
    """A regular, executable file that starts with a linker's object header."""
    try:
        stat = path.lstat()
        if not stat_module.S_ISREG(stat.st_mode) or not stat.st_mode & 0o111:
            return False
        fd = os.open(os.fspath(path), os.O_RDONLY | O_NOFOLLOW)
        try:
            magic = os.read(fd, 4)
        finally:
            os.close(fd)
    except OSError:
        return False
    return magic in NATIVE_EXECUTABLE_MAGICS


def _recognized_cargo_file(subtree: str, parts: tuple[str, ...],
                           path: Optional[Path] = None) -> bool:
    """Recognise Cargo's stable cache names while retaining ambiguous outputs.

    An extensionless name is only a cache file when ``path`` proves it is a
    linked executable; by name alone it is kept.
    """
    if not parts or any(part in ("", ".", "..") for part in parts):
        return False
    name = parts[-1]
    if subtree == "incremental":
        if (len(parts) != 3
                or not re.fullmatch(r"[A-Za-z0-9_]+-[a-z0-9]+", parts[0])
                or not re.fullmatch(r"s-[a-z0-9]+-[a-z0-9]+-[a-z0-9]+", parts[1])):
            return False
        return (name in KNOWN_INCREMENTAL_NAMES
                or name in {"thin-lto-past-keys.bin", "metadata.rmeta"}
                or bool(re.fullmatch(r"[A-Za-z0-9_.-]+\.(?:o|bc)", name)))
    if subtree == ".fingerprint":
        return len(parts) == 2 and _HASHED_DIR.match(parts[0]) and (
            name in KNOWN_FINGERPRINT_NAMES or bool(_FINGERPRINT_METADATA.match(name)))
    if subtree == "build":
        # Files below an `out` directory are build-script products and can be
        # arbitrary user data.  Keep them unless Cargo's own metadata is clear.
        if "out" in parts[:-1]:
            return False
        return len(parts) == 2 and _HASHED_DIR.match(parts[0]) and name in KNOWN_BUILD_NAMES
    if subtree in ("deps", "examples"):
        if len(parts) != 1:
            return False
        if "." not in name:
            return (path is not None and bool(_HASHED_EXECUTABLE.match(name))
                    and _native_executable(path))
        if _RCGU_OBJECT.match(name):
            return True
        return bool(_HASHED_ARTIFACT.match(name)) and name.endswith(tuple(KNOWN_ARTIFACT_SUFFIXES))
    return False


def _identity(path: Path) -> tuple[int, int, int, int, int, int]:
    stat = path.lstat()
    return (stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns, stat.st_mode,
            stat.st_blocks)


def _iter_regular_files(root: Path, errors: Optional[list[str]] = None) -> Iterable[Path]:
    errors = [] if errors is None else errors
    if root.is_symlink() or not root.is_dir():
        return
    try:
        with os.scandir(root) as children:
            for child in children:
                path = Path(child.path)
                if child.is_symlink():
                    continue
                if child.is_file(follow_symlinks=False):
                    yield path
                elif child.is_dir(follow_symlinks=False):
                    yield from _iter_regular_files(path, errors)
    except OSError as error:
        errors.append(f"cannot inspect {root}: {error}")
        return


def plan_cache_cleanup(target: Path) -> CleanupPlan:
    """Build a manifest of exact Cargo cache files below a tagged target."""
    target = _absolute(target)
    refusals: list[str] = []
    if target.is_symlink() or not target.is_dir():
        return CleanupPlan(target, (), 0, ("target is missing or a symlink",))
    if not is_cargo_target(target):
        return CleanupPlan(target, (), 0, ("target has no Cargo marker",))
    entries: list[CacheEntry] = []
    inode_seen: set[tuple[int, int]] = set()
    try:
        profiles = [p for p in target.iterdir() if p.is_dir() and not p.is_symlink() and _profile_dir(p)]
    except OSError as error:
        return CleanupPlan(target, (), 0, (f"cannot inspect target: {error}",))
    for profile in profiles:
        for subtree_name in CACHE_SUBTREES:
            subtree = profile / subtree_name
            if subtree.is_symlink() or not subtree.is_dir():
                continue
            for path in _iter_regular_files(subtree, refusals):
                if not _cache_path(target, path):
                    continue
                try:
                    stat = path.lstat()
                    key = (stat.st_dev, stat.st_ino)
                    allocated = 0 if key in inode_seen else stat.st_blocks * ALLOCATED_BLOCK
                    inode_seen.add(key)
                    entries.append(CacheEntry(path, allocated, _identity(path)))
                except OSError as error:
                    refusals.append(f"cannot inspect {path}: {error}")
    entries.sort(key=lambda entry: str(entry.path))
    return CleanupPlan(target, tuple(entries), sum(entry.size_bytes for entry in entries), tuple(refusals))


def process_snapshot():
    """One fail-closed lsof snapshot, indexed for many session-level checks."""
    try:
        result = subprocess.run(
            ["lsof", "-n", "-P", "-Fpcfn"],
            capture_output=True, text=True, timeout=20, check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return lambda *args, **kwargs: None
    if result.returncode != 0 or not result.stdout or getattr(result, "stderr", "").strip():
        return lambda *args, **kwargs: None
    occupied, cwd_ancestors = set(), set()
    current_pid = current_fd = None
    try:
        for line in result.stdout.splitlines():
            if line.startswith("p"):
                current_pid, current_fd = line[1:], None
            elif line.startswith("f"):
                current_fd = line[1:]
            elif line.startswith("n") and current_pid and current_fd:
                opened = Path(line[1:])
                if not opened.is_absolute():
                    continue
                if current_pid == str(os.getpid()) and opened.name == ".cargo-lock":
                    continue  # The reservation held by this maintenance pass.
                occupied.update((opened, *opened.parents))
                if current_fd == "cwd":
                    cwd_ancestors.update((opened, *opened.parents))
    except (OSError, RuntimeError):
        return lambda *args, **kwargs: None
    if not occupied:
        return lambda *args, **kwargs: None

    def check(path: Path, include_checkout: bool = True) -> bool:
        path = _absolute(path)
        return path in occupied or (include_checkout and path.parent in cwd_ancestors)
    return check


def target_live_status(target: Path, include_checkout: bool = True) -> Optional[bool]:
    """Return live/idle, or None when process inspection is unavailable."""
    return process_snapshot()(target, include_checkout=include_checkout)


def stale_cache_units(plan: CleanupPlan, cutoff: float):
    """Whole known incremental sessions first; other manifest files second, LRU.

    Unknown contents or a recently touched file protect the entire session.
    Build-script products, fixtures and reports never enter the manifest.
    """
    sessions: dict[Path, list[CacheEntry]] = {}
    units = []
    for entry in plan.entries:
        if entry.path.relative_to(plan.target).parts[1] == "incremental":
            sessions.setdefault(entry.path.parent, []).append(entry)
        elif entry.identity[3] / 1e9 <= cutoff:
            units.append((1, entry.identity[3] / 1e9, entry.path, (entry,)))
    for session, entries in sessions.items():
        try:
            if set(session.iterdir()) != {entry.path for entry in entries}:
                continue
            newest = max(session.stat().st_mtime,
                         *(entry.identity[3] / 1e9 for entry in entries))
            if newest <= cutoff:
                units.append((0, newest, session, tuple(entries)))
        except OSError:
            continue
    return sorted(units)


def _remove_empty_session(target: Path, session: Path):
    """Empty-only removal through no-follow fds; never remove its crate directory."""
    parts = _relative_parts(target, session)
    if parts is None or len(parts) != 4 or parts[1] != "incremental":
        return
    with ExitStack() as stack:
        fd = _open_directory(target)
        stack.callback(os.close, fd)
        for component in parts[:-1]:
            fd = _open_directory(Path(component), fd)
            stack.callback(os.close, fd)
        os.rmdir(parts[-1], dir_fd=fd)


def maintain_caches(targets: Iterable[Path], disk_path: Path,
                    reserve_bytes: int = MAINTENANCE_GOAL_BYTES,
                    cap_bytes: Optional[int] = None,
                    same_slot: Optional[Path] = None,
                    process_check=None, free_check=None) -> tuple[int, int, tuple[str, ...]]:
    """Prune stale manifest units, oldest first, stopping at reserve and slot caps.

    Callers select eligible slots. Cargo locks stay held throughout the pass;
    the admitting slot checks each cache unit, preserving open files and sessions
    while allowing unrelated logs to remain open.
    One uncertain or busy target does not prevent reclaiming another idle one.
    """
    reserve_bytes = max(reserve_bytes, MAINTENANCE_GOAL_BYTES)
    cap_bytes = slot_cap_bytes() if cap_bytes is None else cap_bytes
    free = disk_free if free_check is None else free_check
    sizes = {target: target_size(target) for target in dict.fromkeys(targets)}
    available = free(disk_path)
    if available >= reserve_bytes and all(size <= cap_bytes for size in sizes.values()):
        return 0, 0, ()
    removed_bytes = removed_files = 0
    failures = []
    units = []
    cutoff = time.time() - STALE_CACHE_SECONDS
    with ExitStack() as stack:
        for target, size in sizes.items():
            if not size or (available >= reserve_bytes and size <= cap_bytes):
                continue
            handles, error = _acquire_cargo_locks(target)
            for handle in handles:
                stack.callback(handle.close)
            if error:
                failures.append(error)
                continue
            plan = plan_cache_cleanup(target)
            if plan.refusals:
                failures.extend(plan.refusals)
                continue
            units.extend((*unit, target) for unit in stale_cache_units(plan, cutoff))
        # Inspect after planning, with Cargo locks held; one scan keeps large
        # caches from paying a process-table walk for every stale file.
        check = process_snapshot() if process_check is None else process_check
        foreign_status = {}
        reported = set()
        for kind, _mtime, path, entries, target in sorted(units):
            if available >= reserve_bytes and sizes[target] <= cap_bytes:
                continue
            if target == same_slot:
                live = check(path, include_checkout=False)
            else:
                if target not in foreign_status:
                    foreign_status[target] = check(target, include_checkout=True)
                live = foreign_status[target]
            if live is not False:
                if target not in reported:
                    failures.append(f"KEEP cache in {target}: " +
                                    ("live process" if live else "process inspection unavailable"))
                    reported.add(target)
                continue
            # Revalidate the complete unit before removing any of its files.
            try:
                if (any(_identity(entry.path) != entry.identity for entry in entries)
                        or (kind == 0 and (path.stat().st_mtime > cutoff
                            or set(path.iterdir()) != {entry.path for entry in entries}))):
                    continue
            except OSError:
                continue
            for entry in entries:
                removed, error = _unlink_entry(target, entry)
                if removed:
                    removed_bytes += entry.size_bytes
                    removed_files += 1
                    sizes[target] -= entry.size_bytes
                elif error:
                    failures.append(error)
            if kind == 0:
                try:
                    _remove_empty_session(target, path)
                except OSError as error:
                    failures.append(f"kept session directory {path}: {error}")
            available = free(disk_path)
    return removed_bytes, removed_files, tuple(failures)


def _open_directory(path: Path, parent_fd: Optional[int] = None) -> int:
    flags = os.O_RDONLY | O_DIRECTORY | O_NOFOLLOW
    if parent_fd is None:
        path = Path(path)
        if not path.is_absolute() or not O_NOFOLLOW:
            raise OSError(errno.ENOTSUP, "absolute O_NOFOLLOW directory traversal is required")
        # Open each ancestor from the root.  Passing the full string to open()
        # would allow a symlink in an ancestor to redirect the target before
        # the final component's O_NOFOLLOW check runs.
        fd = os.open(os.sep, flags)
        try:
            for component in path.parts[1:]:
                child = os.open(component, flags, dir_fd=fd)
                os.close(fd)
                fd = child
            return fd
        except BaseException:
            os.close(fd)
            raise
    if not O_NOFOLLOW:
        raise OSError(errno.ENOTSUP, "O_NOFOLLOW directory traversal is required")
    return os.open(path.name, flags, dir_fd=parent_fd)


def _relative_parts(target: Path, path: Path) -> Optional[tuple[str, ...]]:
    try:
        relative = _absolute(path).relative_to(_absolute(target))
    except ValueError:
        return None
    parts = relative.parts
    if not parts or any(part in ("", ".", "..") for part in parts):
        return None
    return parts


def _acquire_cargo_locks(target: Path) -> tuple[list[object], Optional[str]]:
    """Acquire existing Cargo lock files without creating or following links."""
    handles: list[object] = []
    candidates: list[Path] = []
    errors: list[str] = []
    for root, dirs, files in os.walk(target, topdown=True, followlinks=False,
                                     onerror=lambda error: errors.append(str(error))):
        root_path = Path(root)
        dirs[:] = [name for name in dirs if not (root_path / name).is_symlink()]
        for name in files:
            if name == ".cargo-lock":
                candidates.append(root_path / name)
    if errors:
        return [], "REFUSED: cannot inspect Cargo lock paths: " + "; ".join(errors)
    for path in candidates:
        handle = None
        try:
            fd = os.open(os.fspath(path), os.O_RDONLY | O_NOFOLLOW)
            handle = os.fdopen(fd, "rb", closefd=True)
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            handles.append(handle)
        except (OSError, IOError) as error:
            if handle is not None:
                handle.close()
            for handle in handles:
                handle.close()
            if getattr(error, "errno", None) in (errno.EACCES, errno.EAGAIN):
                return [], f"REFUSED: Cargo lock is held: {path}"
            return [], f"REFUSED: cannot hold Cargo lock {path}: {error}"
    return handles, None


def _unlink_entry(target: Path, entry: CacheEntry) -> tuple[bool, Optional[str]]:
    """Validate and unlink one entry through O_NOFOLLOW directory fds."""
    parts = _relative_parts(target, entry.path)
    if parts is None or not _cache_path(target, entry.path):
        return False, f"stale or unsafe manifest entry: {entry.path}"
    fds: list[int] = []
    try:
        root_fd = _open_directory(target)
        fds.append(root_fd)
        parent_fd = root_fd
        for component in parts[:-1]:
            child_fd = _open_directory(Path(component), parent_fd)
            fds.append(child_fd)
            parent_fd = child_fd
        name = parts[-1]
        stat = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
        identity = (stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns,
                    stat.st_mode, stat.st_blocks)
        if identity != entry.identity or not stat_module.S_ISREG(stat.st_mode):
            return False, f"changed since manifest: {entry.path}"
        os.unlink(name, dir_fd=parent_fd)
        return True, None
    except OSError as error:
        return False, f"could not safely remove {entry.path}: {error}"
    finally:
        for fd in reversed(fds):
            try:
                os.close(fd)
            except OSError:
                pass


def apply_cache_cleanup(plan: CleanupPlan, dry_run: bool = True,
                        process_check: Optional[Callable[[Path], Optional[bool]]] = None) -> tuple[int, int, tuple[str, ...]]:
    """Apply an exact manifest; return (bytes_removed, files_removed, failures)."""
    check = target_live_status if process_check is None else process_check
    if plan.refusals:
        return 0, 0, plan.refusals
    live = check(plan.target)
    if live is None:
        return 0, 0, ("REFUSED: process inspection unavailable",)
    if live:
        return 0, 0, ("REFUSED: target has a live process",)
    handles, lock_error = _acquire_cargo_locks(plan.target)
    if lock_error:
        return 0, 0, (lock_error,)
    removed_bytes = removed_files = 0
    failures: list[str] = []
    try:
        for entry in plan.entries:
            if dry_run:
                path = entry.path
                if (_cache_path(plan.target, path) and not path.is_symlink()
                        and _regular_file(path) and _identity(path) == entry.identity):
                    removed_bytes += entry.size_bytes
                    removed_files += 1
                else:
                    failures.append(f"stale or unsafe manifest entry: {path}")
                continue
            removed, error = _unlink_entry(plan.target, entry)
            if removed:
                removed_bytes += entry.size_bytes
                removed_files += 1
            elif error:
                failures.append(error)
    finally:
        for handle in handles:
            handle.close()
    return removed_bytes, removed_files, tuple(failures)


def _format_gib(value: int) -> str:
    return f"{value / GIB:.1f}G"


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    check_parser = sub.add_parser("check-build")
    check_parser.add_argument("--repo", type=Path, default=Path.cwd())
    check_parser.add_argument("--target-dir", type=Path, required=True)
    check_parser.add_argument("--full", action="store_true", help="also print the full target inventory")
    args = parser.parse_args(argv)
    if args.command == "check-build":
        result = check_build(args.target_dir, args.repo)
        print(f"TARGET: {result.target}")
        print(f"FREE:   {_format_gib(result.free_bytes)} (reserve {_format_gib(result.reserve_bytes)})")
        print(f"CHECK:  {'OK' if result.ok else result.reason}")
        if args.full:
            report = inventory_targets(args.repo)
            for target in report.targets:
                print(f"INVENTORY: {target.source:9} {target.path} {_format_gib(target.size_bytes)}")
            for error in report.errors:
                print(f"INVENTORY-ERROR: {error}", file=sys.stderr)
            print(f"TOTAL: {_format_gib(report.total_bytes)} / maintenance goal {_format_gib(report.maintenance_goal_bytes)}")
        return 0 if result.ok else 2
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
