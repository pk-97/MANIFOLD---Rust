#!/usr/bin/env python3
"""One-command landing gate (GIT_TREE_DISCIPLINE.md section 2 (Landing protocol)).

Gates only what the branch touched (GPU leg: scripts/gpu_scope.py); the workspace-wide sweep lives in
scripts/trunk_health.py (nightly). Pass --repo <worktree path> of the branch
being landed, after merging origin/main into it. Stop at the first failed check; preserve its
transcript and timings. Exit 0 iff all required checks pass.
"""

import argparse
import contextlib
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

import gpu_queue
import landing_marker
import trunk_health

MAIN_CHECKOUT = Path("/Users/peterkiemann/MANIFOLD - Rust")

# GPU-proofs scope (touched paths -> focused tests + smoke, time budget, no
# run-everything fallback) lives in scripts/gpu_scope.py; the full suite runs
# nightly via trunk_health.py.
import gpu_scope


def build_environment(cmd, cwd):
    """Check each build stage and pin Cargo to its admitted canonical target.

    The gate constructs its own argv; only its known build-driving stages need
    admission. Setting the absolute target also prevents a Cargo config file
    from silently redirecting a gate build to an unmanaged directory.
    """
    builds = ((Path(cmd[0]).name == "cargo" and len(cmd) > 1
               and cmd[1] in {"clippy", "nextest", "test", "check", "build", "run"})
              or any(Path(arg).name in {"gpu_proofs_gate.py", "run_ui_flows.py"}
                     for arg in cmd[:2]))
    if not builds:
        return None, None
    from storage_budget import check_build
    repo = Path(cwd).resolve()
    environment = os.environ.copy()
    overrides = [environment[name] for name in
                 ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR") if environment.get(name)]
    targets = {Path(os.path.abspath(repo / value)) for value in overrides}
    if len(targets) > 1:
        return None, "Storage admission refused: conflicting Cargo target overrides"
    target = next(iter(targets), repo / "target")
    try:
        admission = check_build(target, repo)
    except OSError as error:
        return None, f"Storage admission refused: cannot inspect disk: {error}"
    if not admission:
        return None, "Storage admission refused: " + admission.reason
    environment["CARGO_TARGET_DIR"] = str(admission.target)
    return environment, None


def run_cmd(cmd, cwd, timeout):
    """Run subprocess, return (exit, stdout, stderr, duration).

    A timeout is a FAIL (-1), never a traceback — the gate must always end
    at its summary line."""
    start = time.time()
    environment, refusal = build_environment(cmd, cwd)
    if refusal:
        return 2, "", refusal, time.time() - start
    try:
        r = subprocess.run(cmd, cwd=str(cwd), capture_output=True, text=True,
                           timeout=timeout, env=environment)
    except subprocess.TimeoutExpired as error:
        duration = time.time() - start
        def decoded(value):
            return value.decode(errors="replace") if isinstance(value, bytes) else value or ""
        return (-1, decoded(error.stdout), decoded(error.stderr) +
                f"\nTIMEOUT after {duration:.0f}s: {' '.join(cmd)}", duration)
    duration = time.time() - start
    return r.returncode, r.stdout, r.stderr, duration


def write_landing_log(repo, label, stdout, stderr):
    """Persist the complete subprocess transcript for post-gate diagnosis."""
    log_dir = Path(repo) / "target" / "landing-logs"
    log_dir.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    path = log_dir / f"{label}-{stamp}-{time.time_ns()}.log"
    path.write_text(stdout + stderr)
    return path.resolve()


def run_check(label, cmd, cwd, timeout):
    print(f"[RUN] {label}", flush=True)
    result = run_cmd(cmd, cwd, timeout)
    exit_, out, err, _ = result
    if exit_ and label != "gpu-proofs":
        # GPU proofs retain their transcript on both success and failure below.
        log = write_landing_log(cwd, label.replace("/", "-"), out, err)
        print(f"[{label}] complete transcript: {log}", flush=True)
    return result


def parse_package_from_cargo(toml_path):
    """Extract the first 'name = \"...\"' line from a Cargo.toml."""
    content = Path(toml_path).read_text()
    m = re.search(r'^name\s*=\s*"([^"]+)"', content, re.MULTILINE)
    if m:
        return m.group(1)
    return None

def packages_for_paths(repo, paths):
    packages = []
    for path in paths:
        parts = path.split("/")
        if len(parts) >= 2 and parts[0] == "crates":
            manifest = Path(repo) / "crates" / parts[1] / "Cargo.toml"
            if manifest.exists():
                name = parse_package_from_cargo(manifest)
                if name and name not in packages:
                    packages.append(name)
    return packages


def get_touched_packages(repo, base_sha):
    """Parse changed crate names from diff --name-only."""
    changed = run_cmd(["git", "diff", "--name-only", f"{base_sha}..HEAD"],
                      cwd=repo, timeout=300)[1]
    return packages_for_paths(repo, [line.strip() for line in changed.splitlines() if line.strip()])


def touches_gpu_path(repo, base_sha):
    """Check if diff touches GPU-path files."""
    changed = run_cmd(["git", "diff", "--name-only", f"{base_sha}..HEAD"],
                      cwd=repo, timeout=300)[1]
    return any(gpu_scope.is_gpu_path(line.strip())
               for line in changed.strip().splitlines() if line.strip())


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


NEXTEST_FAIL = re.compile(
    r"^\s*(?:FAIL|SIGABRT|SIGSEGV|SIGBUS|SIGILL|SIGFPE|SIGKILL|SIGTERM|TIMEOUT|LEAK-FAIL|EXEC-FAIL)"
    r"\s+\[[^\]]*\]\s+(\S+)\s+(\S.*?)\s*$")


def parse_nextest_failures(text):
    """Failed (binary_id, test_name) pairs in first-seen order. Nextest prints
    each failure live and again in its summary; retries ("TRY n FAIL") are not
    final results and do not match."""
    seen, failures = set(), []
    for line in text.splitlines():
        m = NEXTEST_FAIL.match(line)
        if m and (m.group(1), m.group(2)) not in seen:
            seen.add((m.group(1), m.group(2)))
            failures.append((m.group(1), m.group(2)))
    return failures


def nextest_filter(failures):
    """Filterset selecting exactly these tests, matched by binary and exact name."""
    def lit(value):
        return re.sub(r"([\\),])", r"\\\1", value)
    return " | ".join(f"(binary_id(={lit(b)}) & test(={lit(t)}))" for b, t in failures)


def failing_on_main(repo, base, failures):
    """Subset of `failures` that also fail in the main checkout, plus a note
    when that could not be established. Main must sit at `base` (origin/main):
    a rerun anywhere else proves nothing about trunk."""
    main_head = run_cmd(["git", "rev-parse", "HEAD"], cwd=MAIN_CHECKOUT, timeout=30)[1].strip()
    base_sha = run_cmd(["git", "rev-parse", base], cwd=repo, timeout=30)[1].strip()
    if not main_head or main_head != base_sha:
        return set(), (f"main checkout is at {main_head[:12] or '?'}, not {base} "
                       f"({base_sha[:12] or '?'}): fast-forward it to rerun the failures there")
    packages = sorted({b.split("::")[0] for b, _ in failures})
    cmd = ["cargo", "nextest", "run", "--no-fail-fast"]
    for package in packages:
        cmd += ["-p", package]
    cmd += ["-E", nextest_filter(failures)]
    print(f"[tests] rerunning {len(failures)} failing test(s) in the main checkout at {base_sha[:12]}",
          flush=True)
    exit_, out, err, _ = run_cmd(cmd, cwd=MAIN_CHECKOUT, timeout=3600)
    log = write_landing_log(repo, "tests-on-main", out, err)
    print(f"[tests] main rerun transcript: {log}", flush=True)
    return set(parse_nextest_failures(out + err)), None


def bead_for_pre_existing(binary, test, branch, base):
    """Id of the open bead naming this test, filing one when none exists.
    Returns (id, None) or (None, why)."""
    try:
        bead = trunk_health.find_open_bead(test)
    except RuntimeError as error:
        return None, str(error)
    if bead:
        return bead, None
    title = f"pre-existing test failure: {test}"[:120]
    desc = (f"landing-gate found {binary} {test} failing on {base} as well as in the branch "
            f"being landed ({branch}). It fails on trunk without the branch's changes. "
            f"Root cause unknown; start from the test's own output.")
    bead, why = trunk_health.file_bead(title, desc)
    if why or not bead:
        return None, why or "bd create returned no id"
    return bead, None


def classify_test_failures(repo, base, branch, output):
    """Split a failed nextest run into new failures (the branch's fault) and
    pre-existing ones. A test is pre-existing only if it fails when rerun in the
    main checkout at origin/main AND an open bead names it. Returns
    (new_failures, pre_existing [(binary, test, bead)], notes)."""
    failures = parse_nextest_failures(output)
    if not failures:
        return [], [], ["no failing test names in the nextest output (build error or crash)"]
    on_main, note = failing_on_main(repo, base, failures)
    notes = [note] if note else []
    new, pre_existing = [], []
    for binary, test in failures:
        if (binary, test) not in on_main:
            new.append((binary, test))
            continue
        bead, why = bead_for_pre_existing(binary, test, branch, base)
        if bead:
            pre_existing.append((binary, test, bead))
        else:
            new.append((binary, test))
            notes.append(f"{test} fails on main but has no bead and none could be filed ({why})")
    return new, pre_existing, notes


def current_branch(repo):
    return run_cmd(["git", "rev-parse", "--abbrev-ref", "HEAD"], cwd=repo, timeout=30)[1].strip()


def skip(results, label, reason):
    """Record a SKIP; the reason travels to the live line and the summary."""
    results.append(("SKIP", label, None, [reason]))
    print(f"[SKIP] {label} ({reason})", flush=True)


def print_result(label, status, duration=None, tail=None):
    """Print [PASS]/[FAIL]/[SKIP] with optional tail."""
    if duration is not None:
        print(f"[{status}] {label} ({duration:.0f}s)", flush=True)
    else:
        print(f"[{status}] {label}", flush=True)
    if tail:
        for line in tail[-20:]:
            print(f"    {line}")


def main():
    with contextlib.ExitStack() as stack:
        return _main(stack)


def _main(stack):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=Path.cwd(),
                        help="worktree of the branch being landed (default: cwd)")
    parser.add_argument("--base", default="origin/main",
                        help="base ref for merge-base (default: origin/main)")
    parser.add_argument("--skip-gpu", default=None, metavar="REASON",
                        help="skip gpu-proofs with a reason (does not fail gate)")
    parser.add_argument("--keep-going", action="store_true",
                        help="collect every result instead of stopping at the first failure "
                             "(diagnosis only: the marker still records a red run)")
    args = parser.parse_args()

    repo = Path(args.repo).resolve()
    base_sha = run_cmd(["git", "merge-base", args.base, "HEAD"],
                       cwd=repo, timeout=300)[1].strip()
    if not base_sha:
        print("[FAIL] merge-base returned empty")
        return 1
    # Agents cannot cd, so a bare run lands in the main checkout where HEAD is
    # the base: nothing is touched, every check skips, exit 0. Two landings on
    # 2026-09-29 merged on that fully-skipped gate.
    head_sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo, timeout=30)[1].strip()
    if head_sha == base_sha:
        print(f"[FAIL] HEAD == {args.base} at {repo}: nothing to gate. "
              "Pass --repo <worktree path> of the branch being landed.")
        return 1

    packages = get_touched_packages(repo, base_sha)
    touches_docs = run_cmd(["git", "diff", "--name-only", "--diff-filter=AR",
                            f"{base_sha}..HEAD", "--", "docs/"],
                           cwd=repo, timeout=300)[1].strip() != ""
    touches_gpu = touches_gpu_path(repo, base_sha)

    results = []
    # Test failures the gate saw: new ones fail the landing, pre-existing ones
    # (red on origin/main, open bead) ride along in the marker.
    ledger = {"failing_tests": [], "pre_existing_tests": []}

    # Harness changes use the same focused tests advertised in worker briefs.
    from codex_checks import tooling_checks
    exit_, changed, err, _ = run_cmd(
        ["git", "diff", "--name-only", "--no-renames", "-z", f"{base_sha}..HEAD"],
        cwd=repo, timeout=300)
    if exit_:
        print(f"[FAIL] harness scope: {err}")
        return 1
    for check in tooling_checks(repo, [p for p in changed.split("\0") if p]):
        exit_, out, err, duration = run_check(check["name"], check["argv"], cwd=repo, timeout=120)
        tail = (out + err).rstrip().splitlines()[-20:]
        status = "PASS" if exit_ == 0 else "FAIL"
        results.append((status, check["name"], duration, tail))
        print_result(check["name"], status, duration, tail if exit_ else None)
        if status == "FAIL" and not args.keep_going:
            return finish(repo, base_sha, results, ledger)

    # a. design-status
    exit_, out, err, duration = run_check("design-status",
        ["python3", ".claude/hooks/design_status_check.py", args.base, "HEAD"],
        cwd=repo, timeout=300)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, "design-status", duration, tail))
    print_result("design-status", status, duration, tail if exit_ != 0 else None)
    if status == "FAIL" and not args.keep_going:
        return finish(repo, base_sha, results, ledger)

    # b. docs-index (only if docs added/renamed)
    if touches_docs:
        exit_, out, err, duration = run_check("docs-index",
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
        if status == "FAIL" and not args.keep_going:
            return finish(repo, base_sha, results, ledger)
    else:
        skip(results, "docs-index", "no docs added or renamed")

    # d. deny
    exit_, out, err, duration = run_check("deny",
        ["cargo", "deny", "check", "bans"],
        cwd=repo, timeout=300)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, "deny", duration, tail))
    print_result("deny", status, duration, tail if exit_ != 0 else None)
    if status == "FAIL" and not args.keep_going:
        return finish(repo, base_sha, results, ledger)

    # d2. ignored-tests — no new #[ignore] beyond the ratchet baseline
    # (spec: .claude/hooks/ignored-test-guard.py docstring).
    exit_, out, err, duration = run_check("ignored-tests",
        ["python3", ".claude/hooks/ignored-test-guard.py", "--scan"],
        cwd=repo, timeout=120)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, "ignored-tests", duration, tail))
    print_result("ignored-tests", status, duration, tail if exit_ != 0 else None)
    if status == "FAIL" and not args.keep_going:
        return finish(repo, base_sha, results, ledger)

    # Extend with direct reverse dependents
    dependents = reverse_deps(repo, packages) if packages else []
    if dependents:
        print(f"dependents added: {', '.join(dependents)}")
    else:
        print("dependents added: none")
    gate_packages = packages + dependents
    # Dedupe while preserving order (touched first, then their dependents)
    seen = set()
    gate_packages = [p for p in gate_packages if not (p in seen or seen.add(p))]

    # e. clippy (if packages touched)
    if gate_packages:
        pkg_args = []
        for p in gate_packages:
            pkg_args.extend(["-p", p])
        cmd = ["cargo", "clippy", *pkg_args, "--tests", "--", "-D", "warnings"]
        exit_, out, err, duration = run_check("clippy", cmd, cwd=repo, timeout=3600)
        tail = (out + err).rstrip().splitlines()[-20:]
        status = "PASS" if exit_ == 0 else "FAIL"
        results.append((status, "clippy", duration, tail))
        print_result("clippy", status, duration, tail if exit_ != 0 else None)
        if status == "FAIL" and not args.keep_going:
            return finish(repo, base_sha, results, ledger)
    else:
        skip(results, "clippy", "no touched packages")

    # c. flow-gate
    exit_, out, err, duration = run_check("flow-gate",
        ["python3", "scripts/run_ui_flows.py", "--touched", f"{base_sha}...HEAD"],
        cwd=repo, timeout=3600)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, "flow-gate", duration, tail))
    print_result("flow-gate", status, duration, tail if exit_ != 0 else None)
    if status == "FAIL" and not args.keep_going:
        return finish(repo, base_sha, results, ledger)

    # Nextest tests call GpuDevice::new_queued; each would queue behind every
    # agent's GPU run on its own. Hold the machine-wide GPU lock once, from
    # here through gpu-proofs: child test processes inherit an ancestor's hold,
    # so the landing waits once (visibly, on stdout) then runs straight through.
    if gate_packages or (touches_gpu and not args.skip_gpu):
        print("[gpu-queue] taking the GPU lock for the tests and gpu-proofs legs", flush=True)
        stack.enter_context(gpu_queue.hold("landing_gate tests+gpu-proofs", out=sys.stdout))

    # f. tests (if packages touched)
    if gate_packages:
        pkg_args = []
        for p in gate_packages:
            pkg_args.extend(["-p", p])
        # --no-fail-fast: classification needs every failing test, not the first.
        cmd = ["cargo", "nextest", "run", "--no-fail-fast", *pkg_args]
        exit_, out, err, duration = run_check("tests", cmd, cwd=repo, timeout=3600)
        tail = (out + err).rstrip().splitlines()[-20:]
        status = "PASS" if exit_ == 0 else "FAIL"
        if exit_ != 0:
            new, pre_existing, notes = classify_test_failures(
                repo, args.base, current_branch(repo), out + err)
            ledger["failing_tests"] += [f"{b} {t}" for b, t in new]
            ledger["pre_existing_tests"] += [
                {"test": f"{b} {t}", "bead": bead} for b, t, bead in pre_existing]
            tail = notes + [f"pre-existing (red on main, bead {bead}): {b} {t}"
                            for b, t, bead in pre_existing] + \
                   [f"NEW failure (passes or is absent on main): {b} {t}" for b, t in new] + tail
            if pre_existing and not new and not notes:
                status = "PASS"
        results.append((status, "tests", duration, tail))
        print_result("tests", status, duration, tail if exit_ != 0 else None)
        if status == "FAIL" and not args.keep_going:
            return finish(repo, base_sha, results, ledger)
    else:
        skip(results, "tests", "no touched packages")

    # g. gpu-proofs
    if touches_gpu:
        if args.skip_gpu:
            skip(results, "gpu-proofs", f"skipped by flag: {args.skip_gpu}")
        else:
            changed = run_cmd(["git", "diff", "--name-only", f"{base_sha}..HEAD"],
                              cwd=repo, timeout=300)[1]
            paths = [l.strip() for l in changed.strip().splitlines() if l.strip()]
            plan = gpu_scope.plan_for_paths(paths, repo)
            if plan.unmapped:
                message = gpu_scope.unmapped_message(plan)
                print(message)
                results.append(("FAIL", "gpu-proofs", None, message.splitlines()))
                return finish(repo, base_sha, results, ledger)
            print("[gpu-proofs] mode: scoped (focused tests + smoke; --all is nightly only)")
            print("[gpu-proofs] " + plan.describe().replace("\n", "\n[gpu-proofs] "), flush=True)
            cmd = ["python3", "scripts/gpu_proofs_gate.py", "--base", args.base,
                   "--budget", str(gpu_scope.LANDING_BUDGET_S)]
            # The GPU hold was taken before the tests leg and is still held.
            exit_, out, err, duration = run_check("gpu-proofs", cmd, cwd=repo, timeout=7200)
            transcript = write_landing_log(repo, "gpu-proofs", out, err)
            print(f"[gpu-proofs] complete transcript: {transcript}")
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
                    if line.startswith(("Failed tests", "Drifted goldens", "Slowest tests")):
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
            if status == "FAIL" and not args.keep_going:
                return finish(repo, base_sha, results, ledger)
    else:
        skip(results, "gpu-proofs", "no GPU paths touched")

    return finish(repo, base_sha, results, ledger)


def finish(repo, base_sha, results, ledger):
    # The marker names HEAD's tree, so the gate must have run on exactly that
    # tree: uncommitted edits to tracked files mean it checked something else.
    dirty = run_cmd(["git", "status", "--porcelain", "--untracked-files=no"],
                    cwd=repo, timeout=60)[1].strip()
    if dirty:
        tail = ["tracked files differ from HEAD, so the gate did not check the tree "
                "that will merge; commit or revert:"] + dirty.splitlines()[:10]
        results.append(("FAIL", "clean-tree", None, tail))
        print_result("clean-tree", "FAIL", None, tail)

    # Summary
    passed = sum(1 for s, _, _, _ in results if s == "PASS")
    failed = sum(1 for s, _, _, _ in results if s == "FAIL")
    skipped = sum(1 for s, _, _, _ in results if s == "SKIP")
    for status, label, duration, tail in results:
        if duration:
            print(f"{status} {label} ({duration:.0f}s)")
        elif status == "SKIP" and tail:
            print(f"{status} {label} ({tail[0]})")
        else:
            print(f"{status} {label}")
    for entry in ledger["pre_existing_tests"]:
        print(f"PRE-EXISTING {entry['test']} (red on origin/main, bead {entry['bead']})")
    for test in ledger["failing_tests"]:
        print(f"NEW FAILURE {test}")
    print(f"landing gate: {passed} passed, {failed} failed, {skipped} skipped")

    # Timing log (JSONL append, main checkout — worktrees come and go)
    branch = current_branch(repo)
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

    # The landing marker: the one fact the merge guard and the landing scripts
    # read. Written on every run, red included, so a stale green can never
    # outlive a newer red for the same checkout.
    passed_gate = failed == 0 and not ledger["failing_tests"]
    tree = run_cmd(["git", "rev-parse", "HEAD^{tree}"], cwd=repo, timeout=30)[1].strip()
    head = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo, timeout=30)[1].strip()
    if not tree:
        print("[FAIL] could not resolve HEAD's tree: no marker written, the landing is not cleared")
        return 1
    record = {
        "schema": landing_marker.SCHEMA,
        "tree": tree,
        "head": head,
        "branch": branch,
        "base": base_sha,
        "pass": passed_gate,
        "failing_tests": ledger["failing_tests"],
        "pre_existing_tests": ledger["pre_existing_tests"],
        "skipped": [f"{label}: {tail[0]}" for status, label, _, tail in results
                    if status == "SKIP" and tail],
        "ts": datetime.now(timezone.utc).isoformat(),
    }
    try:
        marker_path = MAIN_CHECKOUT / ".claude" / "orchestration" / "landing-gate-marker.json"
        landing_marker.write_marker(record, marker_path)
        print(f"landing marker: tree {tree[:12]} {'GREEN' if passed_gate else 'RED'} -> {marker_path}")
    except OSError as e:
        print(f"[FAIL] landing marker not written ({e}): the landing is not cleared")
        return 1

    return 0 if passed_gate else 1


if __name__ == "__main__":
    sys.exit(main())
