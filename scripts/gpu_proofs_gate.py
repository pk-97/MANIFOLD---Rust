#!/usr/bin/env python3
"""GPU-proofs landing gate wrapper — one consolidated drift report.

`cargo test -p manifold-renderer --features gpu-proofs` alone stops at the
first failing test binary, so golden drift surfaces piecemeal over review
rounds. This wrapper streams output live, then parses the full captured run
into one summary: every failed test name, every golden-mismatch detail (file +
diff), and a per-binary pass/fail count. Every selected test binary runs to
completion with `--no-fail-fast`. Never nextest — process-per-test
defeats the GPU device lock.

Default mode is SCOPED: the branch's diff against `--base` (default
origin/main, plus uncommitted and untracked files) is mapped by
scripts/gpu_scope.py to the focused tests for what changed plus a fixed smoke
set. A touched GPU path with no mapping fails loudly; there is no silent
run-everything fallback. `--all` runs the whole suite (nightly trunk_health).
Explicit `--test NAME` / `--filter` / `--skip` bypass scoping for a hand-picked
run. `--budget SECONDS` fails a run whose budgeted tests exceed it and names the
slowest tests. The chosen mode and why are always printed.

Exit 0 iff the underlying cargo run exited 0.

Obsolete when: cargo test reports cross-binary failure summaries natively
and the landing docs point at that instead.
"""

import argparse
import re
import subprocess
import sys
import time
from pathlib import Path

import gpu_scope

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
# Serial runs print each result line when its test finishes, so the gap between
# consecutive result lines is that test's duration (libtest has no stable timing).
TEST_LINE_RE = re.compile(r"^test (\S+) \.\.\. (ok|FAILED)\b")

FAILURES_BLOCK_RE = re.compile(r"failures:\n((?:    \S.*\n)+)\ntest result:")


def default_manifest_path() -> Path:
    return Path(__file__).resolve().parent.parent / "Cargo.toml"


def run_gate(
    manifest_path: Path,
    filters: list[str],
    skips: list[str],
    targets: list[str] | None = None,
    full_suite: bool = False,
    lib: bool = False,
    timings: list | None = None,
) -> tuple[int, str]:
    if full_suite and targets is not None:
        raise ValueError("full_suite and targets are mutually exclusive")

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
    if not full_suite:
        if lib:
            cmd.append("--lib")
        for target in targets or ([] if lib else ["gpu_proofs"]):
            cmd.extend(["--test", target])
    # Serial test threads, always: ~135 proofs share one Metal device, and
    # parallel execution corrupts VALUES, not just timing (BUG-m0c9 — red
    # sets rotate across identical binaries; the same tests pass serially).
    # 15s of determinism is cheaper than the re-run-every-landing tax the
    # parallel mode was paying.
    cmd.append("--")
    cmd.append("--test-threads=1")
    if filters or skips:
        cmd.extend(filters)
        for skip in skips:
            cmd.extend(["--skip", skip])
    print(f"$ {' '.join(cmd)}", flush=True)

    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    lines: list[str] = []
    state: dict = {"t": None, "bin": ""}
    assert proc.stdout is not None
    for line in proc.stdout:
        print(line, end="", flush=True)
        lines.append(line)
        if timings is not None:
            record_timing(line, time.monotonic(), state, timings)
    exit_code = proc.wait()
    return exit_code, "".join(lines)


def record_timing(line: str, now: float, state: dict, timings: list) -> None:
    """Append (test, seconds, binary) when `line` is a finished-test line."""
    m = RUNNING_BINARY_RE.match(line)
    if m:
        state["t"], state["bin"] = now, m.group(1)
        return
    m = TEST_LINE_RE.match(line)
    if m and state.get("t") is not None:
        timings.append((m.group(1), now - state["t"], state["bin"]))
        state["t"] = now


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


def slowest(timings: list, n: int) -> list:
    return sorted(timings, key=lambda t: t[1], reverse=True)[:n]


def budgeted_seconds(timings: list) -> float:
    return sum(t[1] for t in timings if t[3])


def write_timings_md(path: Path, timings: list, n: int = 25) -> None:
    rows = ["# Slowest GPU tests", "", f"{len(timings)} tests, "
            f"{sum(t[1] for t in timings):.0f}s total test time.", "",
            "| # | seconds | test | binary |", "|---|---|---|---|"]
    for i, (name, secs, binary, _) in enumerate(slowest(timings, n), 1):
        rows.append(f"| {i} | {secs:.1f} | `{name}` | {binary} |")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(rows) + "\n")


def print_summary(
    output: str,
    exit_code: int,
    timings: list | None = None,
    budget: float | None = None,
) -> int:
    """Print the consolidated report; return the final exit code."""
    failed_tests = parse_failed_tests(output)
    goldens = parse_golden_mismatches(output)
    binaries = parse_binaries(output)
    timings = timings or []
    spent = budgeted_seconds(timings)
    over_budget = budget is not None and spent > budget

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

    if timings:
        print(f"\nSlowest tests (budgeted test time {spent:.0f}s"
              + (f" of {budget:.0f}s budget" if budget is not None else "") + "):")
        for name, secs, binary, _ in slowest(timings, 10):
            print(f"  - {secs:7.1f}s {name} [{binary}]")

    if binaries:
        print("\nPer-binary results:")
        for label, status, passed, failed in binaries:
            print(f"  - {label}: {status} ({passed} passed, {failed} failed)")
    else:
        print("\nPer-binary results: none parsed")

    print()
    if exit_code == 0 and over_budget:
        print(f"GPU-PROOFS GATE: FAIL (over time budget: {spent:.0f}s > {budget:.0f}s; "
              "fix or split the slowest tests listed above, do not raise the budget)")
        return 3
    if exit_code == 0:
        print("GPU-PROOFS GATE: PASS")
    else:
        print(
            f"GPU-PROOFS GATE: FAIL ({len(failed_tests)} failed tests, "
            f"{len(goldens)} drifted goldens)"
        )
    return exit_code


def git_lines(repo: Path, *args: str) -> list[str]:
    out = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True)
    if out.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {out.stderr.strip()}")
    return [p for p in out.stdout.split("\0") if p]


def changed_paths(repo: Path, base: str) -> list[str]:
    """Branch diff vs merge-base(base, HEAD) plus uncommitted and untracked files."""
    mb = subprocess.run(["git", "-C", str(repo), "merge-base", base, "HEAD"],
                        capture_output=True, text=True)
    if mb.returncode != 0 or not mb.stdout.strip():
        raise RuntimeError(f"cannot resolve merge-base with {base}: {mb.stderr.strip()}; "
                           "fetch it, or pass --base / --path / --all explicitly")
    paths = set(git_lines(repo, "diff", "--name-only", "--no-renames", "-z", f"{mb.stdout.strip()}..HEAD"))
    paths |= set(git_lines(repo, "diff", "--name-only", "--no-renames", "-z", "HEAD"))
    paths |= set(git_lines(repo, "ls-files", "--others", "--exclude-standard", "-z"))
    return sorted(paths)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest-path",
        type=Path,
        default=None,
        help="Path to the workspace Cargo.toml (default: repo root next to scripts/)",
    )
    parser.add_argument(
        "--filter",
        action="append",
        default=[],
        metavar="TESTNAME",
        help="cargo test filter (repeatable); explicit mode, bypasses scoping",
    )
    parser.add_argument(
        "--skip",
        action="append",
        default=[],
        metavar="TESTNAME",
        help="cargo test --skip filter (repeatable)",
    )
    scope = parser.add_mutually_exclusive_group()
    scope.add_argument(
        "--test",
        action="append",
        dest="targets",
        default=None,
        metavar="NAME",
        help="run a named test binary (repeatable); explicit mode, bypasses scoping",
    )
    scope.add_argument(
        "--all",
        "--full-suite",
        action="store_true",
        dest="all_tests",
        help="run every test binary, including glb_conformance (nightly / on demand)",
    )
    parser.add_argument("--base", default="origin/main",
                        help="scoped mode: diff base (default origin/main)")
    parser.add_argument("--path", action="append", default=None, metavar="PATH",
                        help="scoped mode: use these touched paths instead of the git diff")
    parser.add_argument("--budget", type=float, default=None, metavar="SECONDS",
                        help="fail if budgeted test time exceeds this (landing passes "
                        f"{gpu_scope.LANDING_BUDGET_S})")
    parser.add_argument("--timings-md", type=Path, default=None,
                        help="write the 25 slowest tests as markdown to this path")
    args = parser.parse_args()

    manifest_path = args.manifest_path or default_manifest_path()
    repo = manifest_path.parent
    explicit = bool(args.filter or args.skip or args.targets)

    if args.all_tests:
        print("GPU-PROOFS MODE: all (--all: every test binary, no scoping)", flush=True)
        runs = [{"targets": None, "lib": False, "filters": args.filter, "skips": args.skip,
                 "budgeted": False, "full": True}]
    elif explicit:
        print("GPU-PROOFS MODE: explicit (--test/--filter/--skip given; no scoping)", flush=True)
        runs = [{"targets": args.targets, "lib": False, "filters": args.filter,
                 "skips": args.skip, "budgeted": True, "full": False}]
    else:
        try:
            paths = args.path if args.path is not None else changed_paths(repo, args.base)
        except RuntimeError as error:
            print(f"GPU-PROOFS SCOPE: FAIL - {error}")
            return 2
        plan = gpu_scope.plan_for_paths(paths, repo)
        if plan.unmapped:
            print(gpu_scope.unmapped_message(plan))
            return 2
        if not plan.active:
            print(f"GPU-PROOFS MODE: scoped - no GPU paths touched vs {args.base}; nothing to run "
                  "(use --all for the whole suite)")
            return 0
        print("GPU-PROOFS MODE: scoped (default; --all runs the whole suite)\n" + plan.describe(),
              flush=True)
        for note in plan.notes:
            print(f"  note: {note}")
        runs = [dict(r, full=False) for r in plan.runs()]

    exit_code, outputs, all_timings = 0, [], []
    for run in runs:
        run_timings: list = []
        code, output = run_gate(manifest_path, run["filters"], run["skips"], run["targets"],
                                run["full"], run["lib"], run_timings)
        exit_code = exit_code or code
        outputs.append(output)
        all_timings += [(n, s, b, run["budgeted"]) for n, s, b in run_timings]
    output = "".join(outputs)
    if args.timings_md:
        write_timings_md(args.timings_md, all_timings)
    return print_summary(output, exit_code, all_timings, args.budget)


if __name__ == "__main__":
    sys.exit(main())
