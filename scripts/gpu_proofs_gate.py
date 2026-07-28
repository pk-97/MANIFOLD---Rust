#!/usr/bin/env python3
"""GPU-proofs landing gate wrapper — one consolidated drift report.

`cargo test -p manifold-renderer --features gpu-proofs` alone stops at the
first failing test binary, so golden drift surfaces piecemeal over review
rounds. This wrapper runs every test binary to completion
(`--no-fail-fast`), streams the output live, then parses the full captured
run into one summary: every failed test name, every golden-mismatch detail
(file + diff), and a per-binary pass/fail count. Never nextest — process-
per-test defeats the GPU device lock.

Exit 0 iff the underlying cargo run exited 0.

Obsolete when: cargo test reports cross-binary failure summaries natively
and the landing docs point at that instead.
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

# Matches glb_conformance.rs's check_golden() mismatch message:
#   "golden mismatch: mean_abs_diff {mean_abs:.4} > tol {mean_abs_tol} \
#    ({golden_path} vs {rel_file})"
GOLDEN_MISMATCH_RE = re.compile(
    r"golden mismatch: mean_abs_diff ([\d.]+) > tol ([\d.]+) \((.+?) vs (.+?)\)"
)

# cargo prints one of these headers before each test binary's run, e.g.:
#   "     Running tests/glb_conformance.rs (target/debug/deps/glb_conformance-<hash>)"
# The binary path prefix varies with cwd/--manifest-path, so don't anchor on "target".
RUNNING_BINARY_RE = re.compile(r"^\s*Running (\S.*) \((.+)\)\s*$")

# Trailing per-binary summary, e.g. "test result: FAILED. 4 passed; 1 failed; 0 ignored; ..."
TEST_RESULT_RE = re.compile(
    r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed;"
)

# Final failure-name list per binary:
#   failures:
#       test_one
#       test_two
#
#   test result: FAILED. ...
FAILURES_BLOCK_RE = re.compile(r"failures:\n((?:    \S.*\n)+)\ntest result:")


def default_manifest_path() -> Path:
    return Path(__file__).resolve().parent.parent / "Cargo.toml"


def run_gate(manifest_path: Path) -> tuple[int, str]:
    cmd = [
        "cargo",
        "test",
        "-p",
        "manifold-renderer",
        "--features",
        "gpu-proofs",
        "--no-fail-fast",
        "--manifest-path",
        str(manifest_path),
    ]
    print(f"$ {' '.join(cmd)}", flush=True)

    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    lines: list[str] = []
    assert proc.stdout is not None
    for line in proc.stdout:
        print(line, end="", flush=True)
        lines.append(line)
    exit_code = proc.wait()
    return exit_code, "".join(lines)


def parse_binaries(output: str) -> list[tuple[str, str, int, int]]:
    """Return [(binary_label, status, passed, failed), ...] in run order."""
    binaries: list[tuple[str, str, int, int]] = []
    current_label = "(unknown binary)"
    for line in output.splitlines():
        m = RUNNING_BINARY_RE.match(line)
        if m:
            current_label = m.group(1)
            continue
        m = TEST_RESULT_RE.match(line)
        if m:
            status, passed, failed = m.group(1), int(m.group(2)), int(m.group(3))
            binaries.append((current_label, status, passed, failed))
    return binaries


def parse_failed_tests(output: str) -> list[str]:
    names: list[str] = []
    for block in FAILURES_BLOCK_RE.findall(output):
        for line in block.splitlines():
            name = line.strip()
            if name:
                names.append(name)
    return names


def parse_golden_mismatches(output: str) -> list[tuple[str, str, str, str]]:
    """Return [(mean_abs, tol, golden_path, rel_file), ...]."""
    return GOLDEN_MISMATCH_RE.findall(output)


def print_summary(output: str, exit_code: int) -> None:
    failed_tests = parse_failed_tests(output)
    goldens = parse_golden_mismatches(output)
    binaries = parse_binaries(output)

    print("\n" + "=" * 72)
    print("GPU-PROOFS GATE SUMMARY")
    print("=" * 72)

    if failed_tests:
        print(f"\nFailed tests ({len(failed_tests)}):")
        for name in failed_tests:
            print(f"  - {name}")
    else:
        print("\nFailed tests: none")

    if goldens:
        print(f"\nDrifted goldens ({len(goldens)}):")
        for mean_abs, tol, golden_path, rel_file in goldens:
            print(f"  - {rel_file}: mean_abs_diff {mean_abs} > tol {tol} ({golden_path})")
    else:
        print("\nDrifted goldens: none")

    if binaries:
        print("\nPer-binary results:")
        for label, status, passed, failed in binaries:
            print(f"  - {label}: {status} ({passed} passed, {failed} failed)")
    else:
        print("\nPer-binary results: none parsed")

    print()
    if exit_code == 0:
        print("GPU-PROOFS GATE: PASS")
    else:
        print(
            f"GPU-PROOFS GATE: FAIL ({len(failed_tests)} failed tests, "
            f"{len(goldens)} drifted goldens)"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest-path",
        type=Path,
        default=None,
        help="Path to the workspace Cargo.toml (default: repo root next to scripts/)",
    )
    args = parser.parse_args()

    manifest_path = args.manifest_path or default_manifest_path()
    exit_code, output = run_gate(manifest_path)
    print_summary(output, exit_code)
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
