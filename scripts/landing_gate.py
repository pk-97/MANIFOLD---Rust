#!/usr/bin/env python3
"""One-command landing gate (GIT_TREE_DISCIPLINE.md section 2 (Landing protocol)).

Gates only what the branch touched; the workspace-wide sweep lives in
scripts/trunk_health.py (nightly). Run from the worktree being landed, after
merging origin/main into it. Exit 0 iff no check fails.
"""

import argparse
import json
import re
import subprocess
import sys
import time
from pathlib import Path


def run_cmd(cmd, cwd, timeout):
    """Run subprocess, return (exit, stdout, stderr, duration)."""
    start = time.time()
    r = subprocess.run(cmd, cwd=str(cwd), capture_output=True, text=True, timeout=timeout)
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


def touches_gpu_path(repo, base_sha):
    """Check if diff touches GPU-path files (mirrors context-nudge triggers)."""
    changed = run_cmd(["git", "diff", "--name-only", f"{base_sha}..HEAD"],
                      cwd=repo, timeout=300)[1]
    for line in changed.strip().splitlines():
        line = line.strip()
        if line.endswith(".wgsl"):
            return True
        if line.startswith("crates/manifold-gpu/") or line.startswith("crates/manifold-renderer/src/node_graph/"):
            return True
        if "shaders/" in line or "gpu_encoder" in line:
            return True
    return False


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
    touches_docs = run_cmd(["git", "diff", "--name-only", "--diff-filter=AR",
                            f"{base_sha}..HEAD", "--", "docs/"],
                           cwd=repo, timeout=300)[1].strip() != ""
    touches_gpu = touches_gpu_path(repo, base_sha)

    results = []

    # a. design-status
    start = time.time()
    exit_, out, err, duration = run_cmd(
        ["python3", ".claude/hooks/design_status_check.py", args.base, "HEAD"],
        cwd=repo, timeout=300)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, "design-status", duration, tail))
    print_result("design-status", status, duration, tail if exit_ != 0 else None)

    # b. docs-index (only if docs added/renamed)
    if touches_docs:
        start = time.time()
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
    start = time.time()
    exit_, out, err, duration = run_cmd(
        ["python3", "scripts/run_ui_flows.py", "--touched", f"{base_sha}...HEAD"],
        cwd=repo, timeout=3600)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, "flow-gate", duration, tail))
    print_result("flow-gate", status, duration, tail if exit_ != 0 else None)

    # d. deny
    start = time.time()
    exit_, out, err, duration = run_cmd(
        ["cargo", "deny", "check", "bans"],
        cwd=repo, timeout=300)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, "deny", duration, tail))
    print_result("deny", status, duration, tail if exit_ != 0 else None)

    # e. clippy (if packages touched)
    if packages:
        pkg_args = []
        for p in packages:
            pkg_args.extend(["-p", p])
        cmd = ["cargo", "clippy", *pkg_args, "--tests", "--", "-D", "warnings"]
        start = time.time()
        exit_, out, err, duration = run_cmd(cmd, cwd=repo, timeout=3600)
        tail = (out + err).rstrip().splitlines()[-20:]
        status = "PASS" if exit_ == 0 else "FAIL"
        results.append((status, "clippy", duration, tail))
        print_result("clippy", status, duration, tail if exit_ != 0 else None)
    else:
        results.append(("SKIP", "clippy", None, None))
        print_result("clippy", "SKIP", None, None)

    # f. tests (if packages touched)
    if packages:
        pkg_args = []
        for p in packages:
            pkg_args.extend(["-p", p])
        cmd = ["cargo", "nextest", "run", *pkg_args]
        start = time.time()
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
            start = time.time()
            exit_, out, err, duration = run_cmd(
                ["python3", "scripts/gpu_proofs_gate.py"],
                cwd=repo, timeout=7200)
            tail = (out + err).rstrip().splitlines()[-20:]
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
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
