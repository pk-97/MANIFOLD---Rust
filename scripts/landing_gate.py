#!/usr/bin/env python3
"""One-command landing gate (GIT_TREE_DISCIPLINE.md section 2 (Landing protocol)).

Gates only what the branch touched; the workspace-wide sweep lives in
scripts/trunk_health.py (nightly). Run from the worktree being landed, after
merging origin/main into it. Exit 0 iff no check fails.
"""

import argparse
import importlib.util
import json
import re
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

MAIN_CHECKOUT = Path("/Users/peterkiemann/MANIFOLD - Rust")

# GPU-proofs landing scope: narrow the gpu-proofs leg to the subsystem a branch
# touches, so a raytracing-only landing does not pay for the whole ~4.5-min
# suite. Each row is (path-substrings, (filters, skips)); a changed path whose
# substring matches a row contributes that scope, and matching scopes UNION
# (cargo test runs tests matching ANY filter). If no row matches, or any
# GPU-touching path is left uncovered by a narrow scope, the leg falls back to
# the FULL suite — never guess narrow. gpu_proofs_gate.py's default is the full
# suite, so adding a row here is purely additive; the nightly trunk_health
# sweep keeps the full-suite safety net.
#   - `rt_` skips `particletext`: the freeze proof `particletext_*` hangs the
#     GPU on main (BUG-i6eo), so keep it out of RT-scoped runs even though no
#     rt_ test currently matches it.
GPU_PROOFS_SCOPE = [
    (
        (
            "crates/manifold-gpu/src/metal/raytrace.rs",
            "crates/manifold-renderer/src/node_graph/primitives/render_scene.rs",
            "crates/manifold-renderer/src/node_graph/primitives/shaders/render_scene.wgsl",
            "crates/manifold-renderer/tests/gpu_proofs/rt_",
        ),
        (["rt_"], ["particletext"]),
    ),
    (
        ("crates/manifold-renderer/src/node_graph/freeze/",),
        (["freeze::"], []),
    ),
]


def run_cmd(cmd, cwd, timeout):
    """Run subprocess, return (exit, stdout, stderr, duration).

    A timeout is a FAIL (-1), never a traceback — the gate must always end
    at its summary line."""
    start = time.time()
    try:
        r = subprocess.run(cmd, cwd=str(cwd), capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        duration = time.time() - start
        return -1, "", f"TIMEOUT after {duration:.0f}s: {' '.join(cmd)}", duration
    duration = time.time() - start
    return r.returncode, r.stdout, r.stderr, duration


def parse_package_from_cargo(toml_path):
    """Extract the first 'name = \"...\"' line from a Cargo.toml."""
    content = Path(toml_path).read_text()
    m = re.search(r'^name\s*=\s*"([^"]+)"', content, re.MULTILINE)
    if m:
        return m.group(1)
    return None


def get_touched_packages(repo, base_sha):
    """Parse changed crate names from diff --name-only."""
    changed = run_cmd(["git", "diff", "--name-only", f"{base_sha}..HEAD"],
                      cwd=repo, timeout=300)[1]
    packages = []
    seen = set()
    for line in changed.strip().splitlines():
        line = line.strip()
        if not line:
            continue
        parts = line.split("/")
        if len(parts) >= 2 and parts[0] == "crates":
            crate_dir = parts[1]
            manifest = Path(repo) / "crates" / crate_dir / "Cargo.toml"
            if manifest.exists():
                name = parse_package_from_cargo(manifest)
                if name and name not in seen:
                    packages.append(name)
                    seen.add(name)
    return packages


def _path_is_gpu(path):
    """Single-path GPU-trigger predicate.

    Mirrors context-nudge triggers, plus the gpu-proofs test dirs — a change
    to a proof itself (tests/gpu_proofs/rt_*.rs) must run the gpu-proofs leg,
    otherwise the very tests a branch edits never execute at landing.
    """
    if path.endswith(".wgsl"):
        return True
    if path.startswith("crates/manifold-gpu/") or path.startswith("crates/manifold-renderer/src/node_graph/"):
        return True
    if "shaders/" in path or "gpu_encoder" in path:
        return True
    if "tests/gpu_proofs/" in path:
        return True
    return False


def touches_gpu_path(repo, base_sha):
    """Check if diff touches GPU-path files."""
    changed = run_cmd(["git", "diff", "--name-only", f"{base_sha}..HEAD"],
                      cwd=repo, timeout=300)[1]
    return any(_path_is_gpu(line.strip())
               for line in changed.strip().splitlines() if line.strip())


def gpu_proofs_scope_for_paths(paths):
    """Return (filters, skips) for changed `paths`, or None for the FULL suite.

    Union of every GPU_PROOFS_SCOPE row a path matches; None (FULL) when no
    row matches or when any GPU-touching path is left uncovered by a narrow
    scope. Non-GPU paths (scripts/, docs/) never force FULL by themselves.
    """
    filters, skips = [], []
    covered = set()
    for pattern_group, (filters_, skips_) in GPU_PROOFS_SCOPE:
        hits = {p for p in paths if any(pat in p for pat in pattern_group)}
        if hits:
            filters.extend(filters_)
            skips.extend(skips_)
            covered |= hits
    if not filters:
        return None
    gpu_paths = {p for p in paths if _path_is_gpu(p)}
    if gpu_paths - covered:
        return None
    return (sorted(set(filters)), sorted(set(skips)))


def reverse_deps(repo, packages):
    """Find workspace packages that directly depend on any package in packages."""
    try:
        exit_, out, err, duration = run_cmd(
            ["cargo", "metadata", "--format-version", "1"],
            cwd=repo, timeout=120)
        if exit_ != 0:
            print(f"[WARN] cargo metadata failed — gating touched crates only")
            return []

        metadata = json.loads(out)
        # Build map of workspace package names to their direct dependencies
        workspace_members = {}
        for package in metadata.get("packages", []):
            # Only consider workspace members
            if package.get("source") is None:  # workspace packages have no source
                pkg_name = package.get("name")
                deps = set()
                for dep in package.get("dependencies", []):
                    dep_name = dep.get("name")
                    # Only track dependencies that are also workspace members
                    if any(p.get("name") == dep_name and p.get("source") is None
                           for p in metadata.get("packages", [])):
                        deps.add(dep_name)
                workspace_members[pkg_name] = deps

        # Find packages that depend on any of the input packages
        packages_set = set(packages)
        dependents = []
        for pkg_name, deps in workspace_members.items():
            if pkg_name not in packages_set and (deps & packages_set):
                dependents.append(pkg_name)

        return sorted(dependents)
    except Exception as e:
        print(f"[WARN] cargo metadata failed — gating touched crates only")
        return []


def print_result(label, status, duration=None, tail=None):
    """Print [PASS]/[FAIL]/[SKIP] with optional tail."""
    if duration is not None:
        print(f"[{status}] {label} ({duration:.0f}s)")
    else:
        print(f"[{status}] {label}")
    if tail:
        for line in tail[-20:]:
            print(f"    {line}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=Path.cwd(),
                        help="repo path (default: cwd)")
    parser.add_argument("--base", default="origin/main",
                        help="base ref for merge-base (default: origin/main)")
    parser.add_argument("--skip-gpu", default=None, metavar="REASON",
                        help="skip gpu-proofs with a reason (does not fail gate)")
    args = parser.parse_args()

    repo = Path(args.repo).resolve()
    base_sha = run_cmd(["git", "merge-base", args.base, "HEAD"],
                       cwd=repo, timeout=300)[1].strip()
    if not base_sha:
        print("[FAIL] merge-base returned empty")
        return 1

    packages = get_touched_packages(repo, base_sha)
    # Extend with direct reverse dependents
    dependents = reverse_deps(repo, packages)
    if dependents:
        print(f"dependents added: {', '.join(dependents)}")
    else:
        print("dependents added: none")
    gate_packages = packages + dependents
    # Dedupe while preserving order (touched first, then their dependents)
    seen = set()
    gate_packages = [p for p in gate_packages if not (p in seen or seen.add(p))]

    touches_docs = run_cmd(["git", "diff", "--name-only", "--diff-filter=AR",
                            f"{base_sha}..HEAD", "--", "docs/"],
                           cwd=repo, timeout=300)[1].strip() != ""
    touches_gpu = touches_gpu_path(repo, base_sha)

    results = []

    # a. design-status
    exit_, out, err, duration = run_cmd(
        ["python3", ".claude/hooks/design_status_check.py", args.base, "HEAD"],
        cwd=repo, timeout=300)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, "design-status", duration, tail))
    print_result("design-status", status, duration, tail if exit_ != 0 else None)

    # b. docs-index (only if docs added/renamed)
    if touches_docs:
        exit_, out, err, duration = run_cmd(
            ["python3", "scripts/gen_docs_index.py"],
            cwd=repo, timeout=300)
        tail = (out + err).rstrip().splitlines()[-20:]
        stale_check = run_cmd(["git", "diff", "--name-only", "--", "docs/README.md"],
                               cwd=repo, timeout=300)[1].strip()
        if stale_check:
            status = "FAIL"
            tail = ["docs index was stale — commit the regenerated index"]
        else:
            status = "PASS" if exit_ == 0 else "FAIL"
        results.append((status, "docs-index", duration, tail))
        print_result("docs-index", status, duration, tail if status == "FAIL" else None)
    else:
        results.append(("SKIP", "docs-index", None, None))
        print_result("docs-index", "SKIP", None, None)

    # c. flow-gate
    exit_, out, err, duration = run_cmd(
        ["python3", "scripts/run_ui_flows.py", "--touched", f"{base_sha}...HEAD"],
        cwd=repo, timeout=3600)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, "flow-gate", duration, tail))
    print_result("flow-gate", status, duration, tail if exit_ != 0 else None)

    # d. deny
    exit_, out, err, duration = run_cmd(
        ["cargo", "deny", "check", "bans"],
        cwd=repo, timeout=300)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, "deny", duration, tail))
    print_result("deny", status, duration, tail if exit_ != 0 else None)

    # e. clippy (if packages touched)
    if gate_packages:
        pkg_args = []
        for p in gate_packages:
            pkg_args.extend(["-p", p])
        cmd = ["cargo", "clippy", *pkg_args, "--tests", "--", "-D", "warnings"]
        exit_, out, err, duration = run_cmd(cmd, cwd=repo, timeout=3600)
        tail = (out + err).rstrip().splitlines()[-20:]
        status = "PASS" if exit_ == 0 else "FAIL"
        results.append((status, "clippy", duration, tail))
        print_result("clippy", status, duration, tail if exit_ != 0 else None)
    else:
        results.append(("SKIP", "clippy", None, None))
        print_result("clippy", "SKIP", None, None)

    # f. tests (if packages touched)
    if gate_packages:
        pkg_args = []
        for p in gate_packages:
            pkg_args.extend(["-p", p])
        cmd = ["cargo", "nextest", "run", *pkg_args]
        exit_, out, err, duration = run_cmd(cmd, cwd=repo, timeout=3600)
        tail = (out + err).rstrip().splitlines()[-20:]
        status = "PASS" if exit_ == 0 else "FAIL"
        results.append((status, "tests", duration, tail))
        print_result("tests", status, duration, tail if exit_ != 0 else None)
    else:
        results.append(("SKIP", "tests", None, None))
        print_result("tests", "SKIP", None, None)

    # g. gpu-proofs
    if touches_gpu:
        if args.skip_gpu:
            results.append(("SKIP", "gpu-proofs", None, None))
            print(f"[SKIP] gpu-proofs — SKIPPED BY FLAG: {args.skip_gpu}")
        else:
            changed = run_cmd(["git", "diff", "--name-only", f"{base_sha}..HEAD"],
                              cwd=repo, timeout=300)[1]
            paths = [l.strip() for l in changed.strip().splitlines() if l.strip()]
            scope = gpu_proofs_scope_for_paths(paths)
            cmd = ["python3", "scripts/gpu_proofs_gate.py"]
            if scope is None:
                print("[gpu-proofs] full suite (no narrow scope covers the touched paths)")
            else:
                filters, skips = scope
                for f in filters:
                    cmd += ["--filter", f]
                for s in skips:
                    cmd += ["--skip", s]
                print(f"[gpu-proofs] scoped to filters={filters} skips={skips}")
            exit_, out, err, duration = run_cmd(cmd, cwd=repo, timeout=7200)
            # On failure the tail MUST name the failing tests. gpu_proofs_gate's
            # summary prints "Failed tests:"/"Drifted goldens:" ABOVE its
            # per-binary list, so a bare last-20-lines tail scrolls the names
            # out (observed at the R3 landing: a 4-minute rerun just to learn
            # the name). Surface those sections plus the verdict line instead.
            lines = (out + err).rstrip().splitlines()
            if exit_ == 0:
                tail = lines[-20:]
            else:
                names = []
                in_section = False
                for line in lines:
                    if line.startswith(("Failed tests", "Drifted goldens")):
                        in_section = True
                    elif line.startswith(("Per-binary results", "GPU-PROOFS GATE:")):
                        in_section = False
                    if in_section and line.strip():
                        names.append(line)
                verdict = [l for l in lines if l.startswith("GPU-PROOFS GATE:")]
                tail = (names + verdict) or lines[-20:]
            status = "PASS" if exit_ == 0 else "FAIL"
            results.append((status, "gpu-proofs", duration, tail))
            print_result("gpu-proofs", status, duration, tail if exit_ != 0 else None)
    else:
        results.append(("SKIP", "gpu-proofs", None, None))
        print_result("gpu-proofs", "SKIP", None, None)

    # Summary
    passed = sum(1 for s, _, _, _ in results if s == "PASS")
    failed = sum(1 for s, _, _, _ in results if s == "FAIL")
    skipped = sum(1 for s, _, _, _ in results if s == "SKIP")
    for status, label, duration, _ in results:
        if duration:
            print(f"{status} {label} ({duration:.0f}s)")
        else:
            print(f"{status} {label}")
    print(f"landing gate: {passed} passed, {failed} failed, {skipped} skipped")

    # Timing log (JSONL append, main checkout — worktrees come and go)
    branch = run_cmd(["git", "rev-parse", "--abbrev-ref", "HEAD"],
                     cwd=repo, timeout=30)[1].strip()
    try:
        timings_path = MAIN_CHECKOUT / ".claude" / "orchestration" / "landing-gate-timings.jsonl"
        timings_path.parent.mkdir(parents=True, exist_ok=True)
        checks = []
        for status, label, duration, _ in results:
            check_entry = {
                "label": label,
                "status": status,
                "ts": datetime.now(timezone.utc).isoformat(),
                "duration_s": round(duration, 1) if duration is not None else None
            }
            checks.append(check_entry)
        entry = {
            "ts": datetime.now(timezone.utc).isoformat(),
            "branch": branch,
            "checks": checks,
            "failed": failed
        }
        with open(timings_path, "a") as f:
            f.write(json.dumps(entry) + "\n")
    except Exception as e:
        print(f"[WARN] timing log failed: {e}")

    # Self-verdict (only when gate passes)
    if failed == 0:
        try:
            # Extract bead IDs from commit messages
            log_out = run_cmd(["git", "log", f"{base_sha}..HEAD", "--format=%B"],
                             cwd=repo, timeout=300)[1]
            bead_ids = sorted(set(re.findall(r"BUG-\w+", log_out)))
            # Load the MAIN checkout's gate_runner: its append_verdict writes to
            # the main checkout's verdict trail, which is what the merge guard reads.
            gate_runner_path = MAIN_CHECKOUT / "scripts" / "gate_runner.py"
            if bead_ids and not gate_runner_path.exists():
                print(f"[WARN] gate_runner.py not found at {gate_runner_path}")
            elif bead_ids:
                gate_runner_spec = importlib.util.spec_from_file_location(
                    "gate_runner",
                    str(gate_runner_path)
                )
                if gate_runner_spec and gate_runner_spec.loader:
                    gate_runner = importlib.util.module_from_spec(gate_runner_spec)
                    gate_runner_spec.loader.exec_module(gate_runner)
                    commit = run_cmd(["git", "rev-parse", "HEAD"],
                                    cwd=repo, timeout=30)[1].strip()
                    for bead_id in bead_ids:
                        verdict = {
                            "schema": 1,
                            "task": bead_id,
                            "phase": "per-lane",
                            "brief": "scripts/landing_gate.py",
                            "branch": branch,
                            "commit": commit,
                            "gates": [
                                {
                                    "cmd": label,
                                    "exit": 0 if status != "FAIL" else 1,
                                    "duration_s": round(duration, 1) if duration is not None else 0.0,
                                    "tail": status
                                }
                                for status, label, duration, _ in results
                            ],
                            "scope": {"files_changed": [], "in_scope": True},
                            "pass": True,
                            "kind": "gate",
                            "reason": None,
                            "runner": "gate_runner.py@lead",
                            "ts": datetime.now(timezone.utc).isoformat()
                        }
                        gate_runner.append_verdict(bead_id, verdict)
                        print(f"verdict stamped: {bead_id}")
        except Exception as e:
            print(f"[WARN] self-verdict failed: {e}")

    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
