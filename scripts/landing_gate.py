#!/usr/bin/env python3
"""One-command landing gate (GIT_TREE_DISCIPLINE.md section 2 (Landing protocol)).

Gates only what the branch touched (GPU leg: scripts/gpu_scope.py); the workspace-wide sweep lives in
scripts/trunk_health.py (nightly). Pass --repo <worktree path> of the branch
being landed, after merging origin/main into it. Stop at the first failed check; preserve its
transcript and timings. Exit 0 iff all required checks pass.
"""

import argparse
import contextlib
import hashlib
import importlib.util
import json
import os
import re
import signal
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

import gpu_queue

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
        note = ""
        if not admission and admission.reclaimable:
            note = reclaim_landed_caches(admission.reserve_bytes)
            admission = check_build(target, repo)
    except OSError as error:
        return None, f"Storage admission refused: cannot inspect disk: {error}"
    if not admission:
        return None, "Storage admission refused: " + admission.reason + note
    environment["CARGO_TARGET_DIR"] = str(admission.target)
    return environment, None


def reclaim_landed_caches(reserve_bytes):
    """Free landed idle slot caches before refusing a build on the reserve.

    Delegates to the ring (`agent-worktree.py reclaim`), which only ever
    touches target/ of slots that are landed, clean, lease-free and
    process-free. Returns a transcript tail for the refusal message.
    """
    script = Path(__file__).resolve().parent / "agent-worktree.py"
    try:
        out = subprocess.run([sys.executable, str(script), "reclaim",
                              "--free-bytes", str(reserve_bytes)],
                             capture_output=True, text=True, timeout=900)
        text = (out.stdout + out.stderr).strip()
    except (OSError, subprocess.TimeoutExpired) as error:
        text = f"reclaim failed: {error}"
    tail = "\n".join(text.splitlines()[-12:])
    print(f"[storage] reclaim:\n{tail}", flush=True)
    return "\n" + tail


def descendant_pids(root):
    """Every live descendant of `root`, from one `ps` snapshot."""
    try:
        table = subprocess.run(["ps", "-A", "-o", "pid=,ppid="], capture_output=True,
                               text=True, timeout=10).stdout
    except (OSError, subprocess.SubprocessError):
        return []
    children = {}
    for line in table.splitlines():
        fields = line.split()
        if len(fields) == 2 and fields[0].isdigit() and fields[1].isdigit():
            children.setdefault(int(fields[1]), []).append(int(fields[0]))
    found, frontier = [], [root]
    while frontier:
        for child in children.get(frontier.pop(), []):
            found.append(child)
            frontier.append(child)
    return found


def kill_tree(root):
    """SIGKILL `root` and everything under it.

    Killing only the direct child orphans its children: a timed-out flow gate
    left the app running and holding the GPU lock (BUG-i3hc (flow gate
    re-queues the GPU lock per flow)). Each process is stopped before the
    tree is re-read, so nothing can fork past the snapshot."""
    seen = []
    pending = [root]
    while pending:
        for pid in pending:
            with contextlib.suppress(OSError):
                os.kill(pid, signal.SIGSTOP)
        seen.extend(pending)
        pending = [p for p in descendant_pids(root) if p not in seen]
    for pid in seen:
        with contextlib.suppress(OSError):
            os.kill(pid, signal.SIGKILL)


def run_cmd(cmd, cwd, timeout, live_log=None):
    """Run subprocess, return (exit, stdout, stderr, duration).

    With `live_log`, both streams are also written to that file line by line
    as they arrive, so a leg that hangs or is killed still leaves its
    transcript. A timeout is a FAIL (-1) that kills the whole process tree,
    never a traceback — the gate must always end at its summary line."""
    start = time.time()
    environment, refusal = build_environment(cmd, cwd)
    if refusal:
        return 2, "", refusal, time.time() - start
    proc = subprocess.Popen(cmd, cwd=str(cwd), stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True, errors="replace",
                            env=environment)
    streams = {"out": [], "err": []}
    log = open(live_log, "w") if live_log else None
    log_lock = threading.Lock()

    def drain(pipe, sink):
        for line in pipe:
            sink.append(line)
            if log:
                with log_lock:
                    # A reader outliving its join (an escaped process still
                    # holding the pipe) must not write to the closed file.
                    if not log.closed:
                        log.write(line)
                        log.flush()

    readers = [threading.Thread(target=drain, args=(proc.stdout, streams["out"]), daemon=True),
               threading.Thread(target=drain, args=(proc.stderr, streams["err"]), daemon=True)]
    for reader in readers:
        reader.start()
    timed_out = False
    try:
        proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        kill_tree(proc.pid)
        proc.wait()
    except BaseException:
        kill_tree(proc.pid)
        raise
    finally:
        for reader, pipe in zip(readers, (proc.stdout, proc.stderr)):
            reader.join(timeout=30)
            if not reader.is_alive():
                pipe.close()
        if log:
            with log_lock:
                log.close()
    duration = time.time() - start
    out, err = "".join(streams["out"]), "".join(streams["err"])
    if timed_out:
        return -1, out, err + f"\nTIMEOUT after {duration:.0f}s: {' '.join(cmd)}", duration
    return proc.returncode, out, err, duration


def landing_log_path(repo, label):
    log_dir = Path(repo) / "target" / "landing-logs"
    log_dir.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    return (log_dir / f"{label}-{stamp}-{time.time_ns()}.log").resolve()


def write_landing_log(repo, label, stdout, stderr):
    """Persist the complete subprocess transcript for post-gate diagnosis."""
    path = landing_log_path(repo, label)
    path.write_text(stdout + stderr)
    return path


def run_check(label, cmd, cwd, timeout):
    live = landing_log_path(cwd, label.replace("/", "-"))
    print(f"[RUN] {label}  (live transcript: {live})", flush=True)
    result = run_cmd(cmd, cwd, timeout, live_log=live)
    exit_, out, err, _ = result
    if exit_ and label != "gpu-proofs":
        # Rewritten as stdout then stderr, the layout every landing log has.
        # GPU proofs retain their transcript on both success and failure below.
        live.write_text(out + err)
        print(f"[{label}] complete transcript: {live}", flush=True)
    else:
        with contextlib.suppress(OSError):
            live.unlink()
    return result


def build_leg(results, label, cmd, repo):
    """A compile-only leg; records and prints its result, returns the status."""
    exit_, out, err, duration = run_check(label, cmd, cwd=repo, timeout=3600)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, label, duration, tail))
    print_result(label, status, duration, tail if exit_ else None)
    return status


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


THUMBNAIL_KINDS = (("effect-presets", "effects"), ("generator-presets", "generators"))


def stale_thumbnails(repo):
    """Preset JSONs whose thumbnail hash sidecar is missing or differs (pure SHA-256,
    same rule as preset_thumbnail::tests::factory_thumbnails_fresh)."""
    assets = Path(repo) / "crates/manifold-renderer/assets"
    stale = []
    for presets, thumbs in THUMBNAIL_KINDS:
        for preset in sorted((assets / presets).glob("*.json")):
            sidecar = assets / "preset-thumbnails" / thumbs / f"{preset.stem}.hash"
            digest = hashlib.sha256(preset.read_bytes()).hexdigest()
            if not sidecar.is_file() or sidecar.read_text().strip() != digest:
                stale.append(f"{presets}/{preset.name}")
    return stale


def stale_docs_index(repo):
    """True when docs/README.md differs from what gen_docs_index.py would write."""
    docs = Path(repo) / "docs"
    index = docs / "README.md"
    if not index.is_file():
        return False
    import gen_docs_index
    return gen_docs_index.render(docs)[0] != index.read_text(encoding="utf-8")


def freshness_problems(repo):
    """Every stale generated artifact in one pass: (name, detail lines, regenerate command)."""
    problems = []
    thumbs = stale_thumbnails(repo)
    if thumbs:
        shown = thumbs[:10] + ([f"... and {len(thumbs) - 10} more"] if len(thumbs) > 10 else [])
        problems.append(("preset-thumbnails", shown,
                         "cargo run --release -p manifold-renderer --bin generate-preset-thumbnails"))
    if stale_docs_index(repo):
        problems.append(("docs-index", ["docs/README.md differs from the generated index"],
                         "scripts/gen_docs_index.py"))
    return problems


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
                        help="collect every result for explicit named-red review")
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

    # Stale generated artifacts are knowable in seconds; report all of them
    # before any build instead of one per 15-minute rerun.
    problems = freshness_problems(repo)
    if problems:
        for name, detail, command in problems:
            tail = [*detail, f"regenerate: {command}"]
            results.append(("FAIL", f"fresh-{name}", None, tail))
            print_result(f"fresh-{name}", "FAIL", None, tail)
        return finish(repo, base_sha, results)

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
            return finish(repo, base_sha, results)

    # a. design-status
    exit_, out, err, duration = run_check("design-status",
        ["python3", ".claude/hooks/design_status_check.py", args.base, "HEAD"],
        cwd=repo, timeout=300)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, "design-status", duration, tail))
    print_result("design-status", status, duration, tail if exit_ != 0 else None)
    if status == "FAIL" and not args.keep_going:
        return finish(repo, base_sha, results)

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
            return finish(repo, base_sha, results)
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
        return finish(repo, base_sha, results)

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
        return finish(repo, base_sha, results)

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
            return finish(repo, base_sha, results)
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
        return finish(repo, base_sha, results)

    # GPU-proofs scope is settled before anything builds: an unmapped path
    # fails here, not after a compile.
    run_gpu = touches_gpu and not args.skip_gpu
    if run_gpu:
        changed = run_cmd(["git", "diff", "--name-only", f"{base_sha}..HEAD"],
                          cwd=repo, timeout=300)[1]
        paths = [l.strip() for l in changed.strip().splitlines() if l.strip()]
        plan = gpu_scope.plan_for_paths(paths, repo)
        if plan.unmapped:
            message = gpu_scope.unmapped_message(plan)
            print(message)
            results.append(("FAIL", "gpu-proofs", None, message.splitlines()))
            return finish(repo, base_sha, results)

    # Compile every test binary the hold will run before taking it, so the
    # hold covers test time only (BUG-w0hh (landing gate speed)). The legs
    # under the hold then find everything built and go straight to testing.
    if gate_packages:
        pkg_args = []
        for p in gate_packages:
            pkg_args.extend(["-p", p])
        if build_leg(results, "tests-build", ["cargo", "nextest", "run", "--no-run", *pkg_args],
                     repo) == "FAIL" and not args.keep_going:
            return finish(repo, base_sha, results)
    # The catalog check is one nextest test from the binary just built, so it
    # costs seconds and fails before the long test run.
    if "manifold-renderer" in gate_packages:
        exit_, out, err, duration = run_check(
            "catalog-fresh",
            ["cargo", "nextest", "run", "-p", "manifold-renderer", "-E",
             "test(regenerates_in_sync)"], cwd=repo, timeout=600)
        tail = (out + err).rstrip().splitlines()[-20:]
        status = "PASS" if exit_ == 0 else "FAIL"
        if exit_:
            tail.append("regenerate: cargo run -p manifold-renderer --bin gen_node_catalog")
        results.append((status, "catalog-fresh", duration, tail))
        print_result("catalog-fresh", status, duration, tail if exit_ else None)
        if status == "FAIL" and not args.keep_going:
            return finish(repo, base_sha, results)
    if run_gpu:
        if build_leg(results, "gpu-proofs-build",
                     ["python3", "scripts/gpu_proofs_gate.py", "--base", args.base, "--build-only"],
                     repo) == "FAIL" and not args.keep_going:
            return finish(repo, base_sha, results)

    # Nextest tests call GpuDevice::new_queued; each would queue behind every
    # agent's GPU run on its own. Hold the machine-wide GPU lock once, from
    # here through gpu-proofs: child test processes inherit an ancestor's hold,
    # so the landing waits once (visibly, on stdout) then runs straight through.
    if gate_packages or run_gpu:
        print("[gpu-queue] taking the GPU lock for the tests and gpu-proofs legs", flush=True)
        stack.enter_context(gpu_queue.hold("landing_gate tests+gpu-proofs", out=sys.stdout))

    # f. tests (if packages touched)
    if gate_packages:
        pkg_args = []
        for p in gate_packages:
            pkg_args.extend(["-p", p])
        cmd = ["cargo", "nextest", "run", "--no-fail-fast", *pkg_args]
        exit_, out, err, duration = run_check("tests", cmd, cwd=repo, timeout=3600)
        tail = (out + err).rstrip().splitlines()[-20:]
        status = "PASS" if exit_ == 0 else "FAIL"
        results.append((status, "tests", duration, tail))
        print_result("tests", status, duration, tail if exit_ != 0 else None)
        if status == "FAIL" and not args.keep_going:
            return finish(repo, base_sha, results)
    else:
        skip(results, "tests", "no touched packages")

    # g. gpu-proofs
    if touches_gpu:
        if args.skip_gpu:
            skip(results, "gpu-proofs", f"skipped by flag: {args.skip_gpu}")
        else:
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
                return finish(repo, base_sha, results)
    else:
        skip(results, "gpu-proofs", "no GPU paths touched")

    return finish(repo, base_sha, results)


def finish(repo, base_sha, results):
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
