#!/usr/bin/env python3
"""Feature-matrix lint gate — kills the build-rot class (FOUNDATIONAL_GAPS A7).

Non-default features rot because nothing builds them: the workspace clippy
sweep lints only default-feature targets, so a feature-gated test or module
can sit broken on main for weeks (BUG-029 profiling, BUG-033 ui-snapshot,
BUG-hxka gpu-proofs). This script clippy-checks every non-default feature —
build/lint only, never the GPU-run suites — and exits nonzero on any red.

Runs in the nightly trunk-health sweep (scripts/trunk_health.py), not at
landing (GIT_TREE_DISCIPLINE.md section 2 (Landing protocol), 2026-07-29).
Adding a feature to any crate means adding a
row to MATRIX below — the selftest cross-checks MATRIX against the workspace's
Cargo.toml [features] sections, so a new feature that isn't listed (or
exempted with a reason) fails here too.

Usage: scripts/feature_matrix.py [--list]
"""

import argparse
import re
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# (package, feature) pairs to lint. One row per feature; combinations are
# deliberately not exploded — the rot class is "nothing ever builds this",
# not feature interaction.
MATRIX = [
    ("manifold-app", "profiling"),
    ("manifold-app", "ui-snapshot"),
    ("manifold-app", "journey-proofs"),
    ("manifold-app", "perf-soak"),
    ("manifold-core", "bench-timing"),
    ("manifold-gpu", "vulkan"),
    ("manifold-recording", "recording-proofs"),
    ("manifold-renderer", "gpu-proofs"),
    ("manifold-spectral", "gpu-proofs"),
]

# Features that are deliberately not in MATRIX, with the reason. An
# unexempted feature missing from MATRIX fails the coverage check.
EXEMPT = {
    ("manifold-gpu", "default"): "empty default set",
    ("manifold-spectral", "default"): "empty default set",
    ("manifold-spectral", "gpu"): "strict subset of its gpu-proofs row",
}

FEATURES_RE = re.compile(r"^\[features\]\s*$")
SECTION_RE = re.compile(r"^\[")
FEATURE_LINE_RE = re.compile(r"^([A-Za-z0-9_-]+)\s*=")


def workspace_features():
    """Yield (package, feature) for every [features] entry in crates/*/Cargo.toml."""
    for manifest in sorted(REPO.glob("crates/*/Cargo.toml")):
        package = manifest.parent.name
        in_features = False
        for line in manifest.read_text().splitlines():
            if FEATURES_RE.match(line):
                in_features = True
                continue
            if in_features and SECTION_RE.match(line):
                in_features = False
            if in_features:
                m = FEATURE_LINE_RE.match(line)
                if m:
                    yield package, m.group(1)


def check_coverage():
    """Every workspace feature is in MATRIX or EXEMPT; no stale rows."""
    actual = set(workspace_features())
    listed = set(MATRIX) | set(EXEMPT)
    problems = []
    for pair in sorted(actual - listed):
        problems.append(f"{pair[0]} feature {pair[1]!r} is in Cargo.toml but not "
                        "in MATRIX (add a row, or EXEMPT it with a reason)")
    for pair in sorted(listed - actual):
        problems.append(f"{pair[0]} feature {pair[1]!r} is listed here but not "
                        "in any Cargo.toml (remove the stale row)")
    return problems


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--list", action="store_true",
                        help="print the matrix and exit")
    args = parser.parse_args()

    if args.list:
        for package, feature in MATRIX:
            print(f"{package} --features {feature}")
        return 0

    problems = check_coverage()
    for p in problems:
        print(f"[FAIL] coverage — {p}")
    if problems:
        return 1

    failed = []
    for package, feature in MATRIX:
        cmd = ["cargo", "clippy", "-p", package, "--features", feature,
               "--tests", "--", "-D", "warnings"]
        start = time.time()
        r = subprocess.run(cmd, cwd=str(REPO), capture_output=True, text=True)
        duration = time.time() - start
        status = "PASS" if r.returncode == 0 else "FAIL"
        print(f"[{status}] {package} --features {feature} ({duration:.0f}s)")
        if r.returncode != 0:
            failed.append((package, feature))
            tail = (r.stdout + r.stderr).rstrip().splitlines()[-25:]
            print("\n".join(f"    {line}" for line in tail))

    if failed:
        names = ", ".join(f"{p} +{f}" for p, f in failed)
        print(f"feature matrix RED: {names}")
        return 1
    print(f"feature matrix green: {len(MATRIX)} rows")
    return 0


if __name__ == "__main__":
    sys.exit(main())
