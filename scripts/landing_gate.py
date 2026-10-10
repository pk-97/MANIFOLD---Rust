#!/usr/bin/env python3
"""One-command landing gate (GIT_TREE_DISCIPLINE.md section 2 (Landing protocol)).

Gates only what the branch touched (GPU leg: scripts/gpu_scope.py); the workspace-wide sweep lives in
scripts/trunk_health.py (nightly). Pass --repo <worktree path> of the branch
being landed, after merging origin/main into it. Collect cheap readiness errors
before any build, then collect runtime failures with transcripts and timings.
Compiled test-inventory validation necessarily happens after compilation.
Exit 0 iff all required checks pass; exit CHECKS_RED
only when every required check ran and some were red; exit 1 on a
refusal, a crash, or a `--fail-fast` stop.
"""

import argparse
import codecs
import contextlib
import contextvars
import importlib.util
import io
import json
import math
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
from gate_cancellation import Cancelled, cancellation_signals
from gpu_proofs_gate import INPUTS_CHANGED as PROOF_INPUTS_CHANGED

MAIN_CHECKOUT = Path("/Users/peterkiemann/MANIFOLD - Rust")
GATED_HEAD = contextvars.ContextVar('gated_head', default=None)
# Exit code of a gate that ran every required check on a tree that
# held still and found some red: the only red land_branch.py may land over
# with an explicit named red. Refusals, crashes and --fail-fast stops exit 1.
CHECKS_RED = 4
RAN_EVERY_CHECK = contextvars.ContextVar('ran_every_check', default=False)
# Seconds this gate spent waiting for the machine-wide GPU lock; None when no
# leg needed it. Logged per run: queue time is landing time nobody saw before.
GPU_WAIT = contextvars.ContextVar('gpu_wait', default=None)
SLOW_TESTS = contextvars.ContextVar('slow_tests', default=None)
# Set by run_check while its child is active so run_cmd can report the
# deadline before asking a child to handle SIGTERM.  Keeping this in context
# avoids changing run_cmd's established return shape or call signature.
ACTIVE_CHECK = contextvars.ContextVar('active_check', default=None)

# GPU-proofs scope (touched paths -> focused tests + smoke, time budget, no
# run-everything fallback) lives in scripts/gpu_scope.py; the full suite runs
# nightly via trunk_health.py.
import gpu_scope
import cpu_scope
import diff_scope
import gate_passes
import gate_readiness
from gate_workspace import Workspace


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
    environment["CARGO_INCREMENTAL"] = "0"
    environment["CARGO_BUILD_JOBS"] = "4"
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


def stop_child(proc, graceful=False):
    """Reap our child; nested gates get time to release their own holds."""
    if graceful and proc.poll() is None:
        with contextlib.suppress(OSError):
            proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=5)
            with contextlib.suppress(ProcessLookupError):
                os.killpg(proc.pid, signal.SIGKILL)
            return
        except subprocess.TimeoutExpired:
            if getattr(proc, '_requires_cleanup', False):
                # The proof runner owns a separate Cargo session. Killing the
                # runner would discard the only reliable handle to that group
                # when ps is unavailable. Retain ownership until it reaps it.
                print('[cancellation] waiting for proof descendant cleanup; GPU ownership retained', flush=True)
                proc.wait()
                return
    kill_tree(proc.pid)
    with contextlib.suppress(OSError):
        os.killpg(proc.pid, signal.SIGKILL)
    proc.wait()


def record_incomplete(repo, error):
    message = f"[INCOMPLETE] landing gate: cancelled by {error}; children stopped"
    path = write_landing_log(repo, 'incomplete', message + '\n', '')
    print(f"{message} (transcript: {path})", flush=True)
    return 128 + error.signum


def run_cmd(cmd, cwd, timeout, live_log=None):
    """Run subprocess, return (exit, stdout, stderr, duration).

    With `live_log`, both streams are also written to that file in chunks
    as they arrive, so a leg that hangs or is killed still leaves its
    transcript. A timeout is a FAIL (-1) that kills the whole process tree,
    never a traceback — the gate must always end at its summary line."""
    start = time.time()
    environment, refusal = build_environment(cmd, cwd)
    if refusal:
        return 2, "", refusal, time.time() - start
    proc = subprocess.Popen(cmd, cwd=str(cwd), stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True, errors="replace",
                            env=environment, start_new_session=True)
    proc._requires_cleanup = any(Path(str(part)).name == 'gpu_proofs_gate.py' for part in cmd)
    streams = {"out": [], "err": []}
    log = open(live_log, "w") if live_log else None
    log_lock = threading.Lock()

    def drain(pipe, sink):
        decoder = codecs.getincrementaldecoder('utf-8')(errors='replace')
        while True:
            data = pipe.buffer.read1(65536)
            chunk = decoder.decode(data, final=not data)
            sink.append(chunk)
            if log:
                with log_lock:
                    # A reader outliving its join (an escaped process still
                    # holding the pipe) must not write to the closed file.
                    if not log.closed:
                        log.write(chunk)
                        log.flush()
            if not data:
                break

    readers = [threading.Thread(target=drain, args=(proc.stdout, streams["out"]), daemon=True),
               threading.Thread(target=drain, args=(proc.stderr, streams["err"]), daemon=True)]
    for reader in readers:
        reader.start()
    timed_out = False
    try:
        proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        active_check = ACTIVE_CHECK.get()
        if active_check is not None:
            label, configured_timeout = active_check
            print(f"[FAIL] {label} timed out at configured "
                  f"{configured_timeout:g} seconds", flush=True)
        stop_child(proc, graceful=True)
    except BaseException:
        stop_child(proc, graceful=True)
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
        return -1, out, err + (
            f"\nTIMEOUT after {duration:.0f}s: {' '.join(cmd)}"
            f"\nTIMEOUT: timed out at configured {timeout:g} seconds"), duration
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


def parse_slow_tests(output):
    """Keep one maximum per binary/test, including unfinished SLOW lower bounds."""
    if not isinstance(output, str):
        return []
    output = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', output)
    times = {}
    for line in output.splitlines():
        match = re.fullmatch(
            r'\s*(?:SLOW|PASS)\s+\[\s*>?\s*([0-9]+(?:\.[0-9]+)?)s\]\s+'
            r'(?:\(\d+/\d+\)\s+)?(\S+)\s+(\S.*?)\s*', line)
        if not match:
            continue
        seconds = float(match[1])
        if not math.isfinite(seconds) or seconds < 10:
            continue
        name = f'{match[2]} {match[3]}'
        times[name] = max(times.get(name, 0), seconds)
    return [{'name': name, 's': seconds} for name, seconds in
            sorted(times.items(), key=lambda item: (-item[1], item[0]))[:10]]


def run_check(label, cmd, cwd, timeout, passed=None):
    nextest = len(cmd) > 1 and Path(cmd[0]).name == 'cargo' and cmd[1] == 'nextest'
    slow_tests = SLOW_TESTS.get()
    if nextest and slow_tests is not None:
        slow_tests[label] = []
    # Proofs own their canonical per-invocation records (also used by gpu_queue).
    # Build receipts alone cannot guarantee artifacts still exist after reclaim.
    cacheable = (label != 'gpu-proofs' and '--no-run' not in cmd
                 and '--build-only' not in cmd)
    if passed and gate_passes.changed_passes([passed]):
        RAN_EVERY_CHECK.set(False)
        return 1, '', 'inputs changed after planning; rerun the gate\nrerun: ' + rerun_command(
            ['scripts/landing_gate.py', '--repo', str(cwd)], cwd), 0.0
    passed = passed or (gate_passes.command_pass(cwd, label, cmd) if cacheable else None)
    if passed and passed.reused():
        if label == 'flow-gate':
            import run_ui_flows
            previous = run_ui_flows.ROOT
            try:
                run_ui_flows.ROOT = str(cwd)
                manifest = json.loads((Path(cwd) / 'scripts/ui-flows/manifest.json').read_text())
                filters, _ = run_ui_flows.filters_for_touched(cmd[-1], manifest)
                run_ui_flows.write_gate_marker(cmd[-1], filters, True)
            finally:
                run_ui_flows.ROOT = previous
        return 0, '[REUSED] ' + label, '', 0.0
    live = landing_log_path(cwd, label.replace("/", "-"))
    print(f"[RUN] {label}  (live transcript: {live})", flush=True)
    token = ACTIVE_CHECK.set((label, timeout))
    try:
        result = run_cmd(cmd, cwd, timeout, live_log=live)
    finally:
        ACTIVE_CHECK.reset(token)
    exit_, out, err, seconds = result
    timed_out = exit_ == -1
    proof_refusal = (exit_ == PROOF_INPUTS_CHANGED and len(cmd) > 1
                     and Path(cmd[1]).name == 'gpu_proofs_gate.py')
    # A timing-only red (every test passed, rerun done) is a check that ran.
    timing_fail = ('GPU-PROOFS TIMING: FAIL' in out + err
                   and 'GPU-PROOFS TIMING: ONLY' not in out + err)
    if exit_ == -1 or proof_refusal or timing_fail or 'GPU-PROOFS GATE: HUNG' in out + err:
        # Keep collecting runtime reds, but missing coverage cannot be waived
        # through the named-red landing path.
        RAN_EVERY_CHECK.set(False)
    if nextest and slow_tests is not None:
        slow_tests[label] = parse_slow_tests(out + '\n' + err)
    if timed_out:
        # The child output remains available in `live`.  Do not return it as
        # the leg diagnostic: a nested test may print an assertion while
        # handling our SIGTERM, which must not look like the timeout's cause.
        out = ''
        err = (f"TIMEOUT: timed out at configured {timeout:g} seconds\n"
               f"raw transcript: {live}")
    if label == 'docs-index' and exit_ == 0:
        stale = run_cmd(['git', 'diff', '--name-only', '--', 'docs/README.md'],
                        cwd=cwd, timeout=300)[1].strip()
        if stale:
            exit_ = 1
            err += '\ndocs index was stale — commit the regenerated index'
            result = exit_, out, err, seconds
    if passed and not timed_out:
        failed = gate_passes.parse_failed_tests(out + '\n' + err) if exit_ else None
        if passed.save(exit_, seconds, failed=failed) is False:
            RAN_EVERY_CHECK.set(False)
            exit_ = 1
            err += '\ninputs changed during receipt publication; rerun the gate'
            result = exit_, out, err, seconds
    if exit_ and (label != "gpu-proofs" or timed_out):
        # Rewritten as stdout then stderr, the layout every landing log has.
        # GPU proofs retain their transcript on both success and failure below.
        # A timeout's live file was written incrementally by run_cmd and is
        # deliberately kept as the raw child transcript; the timeout marker
        # remains in the returned diagnostic tail.
        if exit_ != -1:
            live.write_text(out + err)
        print(f"[{label}] complete transcript: {live}", flush=True)
    else:
        with contextlib.suppress(OSError):
            live.unlink()
    if exit_:
        # After the transcript is written: a leg that names its own failures
        # (gpu-proofs, flow-gate) prints `rerun:` lines; otherwise the leg's
        # command is the rerun.
        reruns = [line for line in (out + err).splitlines() if line.startswith("rerun: ")]
        err += "\n" + "\n".join(reruns or [f"rerun: {rerun_command(cmd, cwd)}"])
        result = exit_, out, err, seconds
    return result


def build_leg(results, label, cmd, repo):
    """A compile-only leg; records and prints its result, returns the status."""
    exit_, out, err, duration = run_check(label, cmd, cwd=repo, timeout=3600)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, label, duration, tail))
    print_result(label, status, duration, tail if exit_ else None)
    return status


def packages_for_paths(repo, paths):
    workspace = Workspace(repo)
    return sorted({workspace.owner(path) for path in paths} - {None})


def reverse_deps(repo, packages):
    return Workspace(repo).reverse_dependencies(packages)


def flow_filters(repo, paths):
    """The flow-name filters the flow gate would run for the touched `paths`;
    [] when nothing flow-mapped was touched (a missing manifest maps nothing)."""
    import run_ui_flows
    manifest_path = Path(repo) / 'scripts/ui-flows/manifest.json'
    manifest = json.loads(manifest_path.read_text()) if manifest_path.is_file() else {}
    return run_ui_flows.filters_for_paths(paths, manifest)[0]


def rerun_command(cmd, repo):
    """`cmd` as an agent can run it from any cwd: cargo gets --manifest-path,
    repo scripts run by absolute path (no interpreter prefix)."""
    repo = Path(repo).resolve()
    if cmd[0] == "cargo" and "--manifest-path" not in cmd:
        cmd = [*cmd[:2], "--manifest-path", str(repo / "Cargo.toml"), *cmd[2:]]
    elif cmd[0] in {"python3", sys.executable} and len(cmd) > 1 and cmd[1].endswith(".py"):
        cmd = [str(repo / cmd[1]), *cmd[2:]]
    return " ".join(part if " " not in part else repr(part) for part in cmd)


def stale_docs_index(repo):
    """True when docs/README.md differs from what gen_docs_index.py would write."""
    docs = Path(repo) / "docs"
    index = docs / "README.md"
    if not index.is_file():
        return False
    import gen_docs_index
    return gen_docs_index.render(docs)[0] != index.read_text(encoding="utf-8")


def freshness_problems(repo):
    """Every stale generated artifact in one pass: (name, detail lines, regenerate command).
    Preset thumbnails are not gated: a stale one only shows an old picture."""
    problems = []
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
    timeout = timeout_detail(tail)
    if timeout is not None:
        print(f"[FAIL] {label} timed out at configured {timeout} seconds", flush=True)
    elif duration is not None:
        print(f"[{status}] {label} ({duration:.0f}s)", flush=True)
    else:
        print(f"[{status}] {label}", flush=True)
    if tail:
        for line in tail[-20:]:
            print(f"    {line}")


def timeout_detail(tail):
    """Return configured timeout seconds from a run_check diagnostic tail."""
    for line in tail or ():
        match = re.match(r'^TIMEOUT: timed out at configured ([0-9]+(?:\.[0-9]+)?) seconds$', line)
        if match:
            return match[1]
    return None


def main():
    token = GATED_HEAD.set(None)
    ran = RAN_EVERY_CHECK.set(False)
    wait = GPU_WAIT.set(None)
    slow = SLOW_TESTS.set({})
    try:
        with cancellation_signals(), contextlib.ExitStack() as stack:
            stack.enter_context(gate_passes.session())
            return _main(stack)
    except Cancelled as error:
        RAN_EVERY_CHECK.set(False)
        repo = Path(sys.argv[sys.argv.index('--repo') + 1]) if '--repo' in sys.argv else Path.cwd()
        return record_incomplete(repo, error)
    finally:
        SLOW_TESTS.reset(slow)
        GPU_WAIT.reset(wait)
        RAN_EVERY_CHECK.reset(ran)
        GATED_HEAD.reset(token)


def _main(stack):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=Path.cwd(),
                        help="worktree of the branch being landed (default: cwd)")
    parser.add_argument("--base", default="origin/main",
                        help="base ref for merge-base (default: origin/main)")
    parser.add_argument("--skip-gpu", default=None, metavar="REASON",
                        help="skip gpu-proofs with a reason (does not fail gate)")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--keep-going", action="store_true",
                      help="diagnostic: run expensive legs even after cheap reds")
    mode.add_argument("--fail-fast", action="store_true",
                      help="stop at the first red")
    parser.add_argument('--plan-only', action='store_true', help='read-only plan including working-tree changes; no build, test or receipt writes')
    parser.add_argument('--paths-json', type=Path, help='plan-only path fixture with paths/base/head')
    args = parser.parse_args()

    if args.paths_json and not args.plan_only:
        parser.error('--paths-json requires --plan-only')
    planning_started = time.perf_counter()
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
    if head_sha == base_sha and not args.plan_only:
        print(f"[FAIL] HEAD == {args.base} at {repo}: nothing to gate. "
              "Pass --repo <worktree path> of the branch being landed.")
        return 1
    dirty = run_cmd(['git', 'status', '--porcelain', '--untracked-files=normal'],
                    cwd=repo, timeout=30)
    if (dirty[0] or dirty[1].strip()) and not args.plan_only:
        print('[FAIL] landing needs a clean committed tree; standalone proof passes can precede the commit')
        return 1
    GATED_HEAD.set(None if args.plan_only else head_sha)
    RAN_EVERY_CHECK.set(not args.fail_fast)

    try:
        if args.paths_json:
            fixture = json.loads(args.paths_json.read_text())
            paths, ignored_paths = fixture['paths'], []
            base_sha = fixture['base']
        else:
            paths, ignored_paths = diff_scope.effective_paths(repo, base_sha, head=None if args.plan_only else 'HEAD')
            if args.plan_only:
                paths = sorted(set(paths) | set(diff_scope.git(repo, 'ls-files', '--others', '--exclude-standard', '-z').strip('\0').split('\0')) - {''})
    except RuntimeError as error:
        print(f"[FAIL] diff scope: {error}")
        return 1
    if ignored_paths:
        print(f"[scope] excluded {len(ignored_paths)} docs/comment-only file(s)")
    readiness = gate_readiness.plan(repo, paths, base_sha)
    packages, dependents = readiness['packages'], readiness['dependents']
    cpu_plan = readiness['cpu'] or cpu_scope.Plan()
    plan = readiness['gpu'] or gpu_scope.Plan()
    touches_gpu = bool(plan and plan.active)
    nested_analyzer_paths = set(gate_readiness.analyzer_paths(paths))
    root_gpu_paths = [path for path in paths if path not in nested_analyzer_paths]
    scope_reason = "docs/comment-only diff" if not paths else "no touched packages"
    results = []
    readiness_cmd = [str(repo / 'scripts/landing_gate.py'), '--repo', str(repo),
                     '--base', args.base, '--plan-only']
    if args.paths_json:
        readiness_cmd += ['--paths-json', str(args.paths_json.resolve())]
    for label, message in readiness['errors']:
        tail = [*message.splitlines(), f'rerun: {rerun_command(readiness_cmd, repo)}']
        results.append(('FAIL', label, None, tail))
        print_result(label, 'FAIL', tail=tail)
        if args.fail_fast and not args.plan_only:
            return refuse(repo, base_sha, results)
    if readiness['errors']:
        RAN_EVERY_CHECK.set(False)
    try:
        freshness = freshness_problems(repo)
    except (OSError, ValueError, KeyError, TypeError, RuntimeError) as error:
        freshness = [('docs-index', [str(error)], 'scripts/gen_docs_index.py')]
    for name, detail, command in freshness:
        tail = [*detail, f'rerun: {rerun_command([command], repo)}']
        results.append(('FAIL', 'fresh-' + name, None, tail))
        print_result('fresh-' + name, 'FAIL', tail=tail)
        if args.fail_fast and not args.plan_only:
            return refuse(repo, base_sha, results)
    planning_seconds = time.perf_counter() - planning_started
    print(f'[planning] {planning_seconds:.3f}s', flush=True)
    if args.plan_only:
        print(json.dumps(dict(gate_readiness.describe(readiness), planning_seconds=planning_seconds), indent=2))
        return int(any(row[0] == 'FAIL' for row in results))

    # Execute the tooling selection already validated by readiness.
    tooling = readiness['tooling']
    gpu_tooling = [check for check in tooling if check.get('phase') == 'gpu']
    for check in tooling:
        if check.get('phase') == 'gpu':
            continue
        exit_, out, err, duration = run_check(
            check["name"], check["argv"], cwd=repo,
            timeout=check.get("timeout", 120))
        tail = (out + err).rstrip().splitlines()[-20:]
        status = "PASS" if exit_ == 0 else "FAIL"
        results.append((status, check["name"], duration, tail))
        print_result(check["name"], status, duration, tail if exit_ else None)
        if exit_ and args.fail_fast:
            return refuse(repo, base_sha, results)

    # a. design-status: execute and fingerprint the same base pinned at start.
    exit_, out, err, duration = run_check("design-status",
        ["python3", ".claude/hooks/design_status_check.py", base_sha, "HEAD"],
        cwd=repo, timeout=300)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, "design-status", duration, tail))
    print_result("design-status", status, duration, tail if exit_ != 0 else None)
    if exit_ and args.fail_fast:
        return refuse(repo, base_sha, results)

    # d2. ignored-tests — no new #[ignore] beyond the ratchet baseline
    # (spec: .claude/hooks/ignored-test-guard.py docstring).
    exit_, out, err, duration = run_check("ignored-tests",
        ["python3", ".claude/hooks/ignored-test-guard.py", "--scan"],
        cwd=repo, timeout=120)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, "ignored-tests", duration, tail))
    print_result("ignored-tests", status, duration, tail if exit_ != 0 else None)

    if exit_ and args.fail_fast:
        return refuse(repo, base_sha, results)
    stack.enter_context(gpu_queue.landing_pending())

    # d. deny
    exit_, out, err, duration = run_check("deny",
        ["cargo", "deny", "check", "bans"],
        cwd=repo, timeout=300)
    tail = (out + err).rstrip().splitlines()[-20:]
    status = "PASS" if exit_ == 0 else "FAIL"
    results.append((status, "deny", duration, tail))
    print_result("deny", status, duration, tail if exit_ != 0 else None)
    if status == "FAIL" and args.fail_fast:
        return finish(repo, base_sha, results)

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
        for package in gate_packages:
            label = 'clippy' if len(gate_packages) == 1 else f'clippy/{package}'
            cmd = ['cargo', 'clippy', '-p', package, '--tests', '--', '-D', 'warnings']
            exit_, out, err, duration = run_check(label, cmd, cwd=repo, timeout=3600)
            tail = (out + err).rstrip().splitlines()[-20:]
            status = 'PASS' if exit_ == 0 else 'FAIL'
            results.append((status, label, duration, tail))
            print_result(label, status, duration, tail if exit_ else None)
            if status == 'FAIL' and args.fail_fast:
                return finish(repo, base_sha, results)
    else:
        skip(results, "clippy", scope_reason)

    # c. flow-gate uses the same comment-aware diff and writes its landing
    # marker even when no flows need a build or run. Its binary compiles
    # before the GPU hold and its flows run under the hold with the tests and
    # proofs: one wait per landing, and no leg's timeout counts queue time
    # (both hour-long flow-gate reds on 2026-10-06 were waits behind a
    # 45-minute render).
    flow_cmd = ["python3", "scripts/run_ui_flows.py", "--touched", f"{base_sha}...HEAD"]
    flow_pass = gate_passes.command_pass(repo, "flow-gate", flow_cmd)
    flows_pending = (readiness['flows'] and not flow_pass.record)

    def flow_leg():
        exit_, out, err, duration = run_check("flow-gate", flow_cmd, cwd=repo, timeout=3600, passed=flow_pass)
        tail = (out + err).rstrip().splitlines()[-20:]
        status = "PASS" if exit_ == 0 else "FAIL"
        results.append((status, "flow-gate", duration, tail))
        print_result("flow-gate", status, duration, tail if exit_ != 0 else None)
        return status

    if not paths:
        if expensive_blocked(args, results):
            return refuse(repo, base_sha, results)
        flow_leg()
        for label in ("tests-build", "catalog-fresh", "gpu-proofs-build", "tests", "gpu-proofs"):
            skip(results, label, scope_reason)
        return finish(repo, base_sha, results)

    run_gpu = touches_gpu and not plan.unmapped and not args.skip_gpu

    # Nextest must keep the default-feature test set, regardless of proof
    # scope. Enabling gpu-proofs also admits nested/individually gated tests
    # and required-features binaries; their names have no common boundary.
    # An exact default inventory would require its own build anyway. Keep
    # separate feature builds and let gpu_proofs_gate alone opt into proofs
    # via scoped, budgeted cargo test runs.

    test_legs = []
    for package, filterset in cpu_plan.selections().items():
        label = f'tests/{package}'
        cmd = ['cargo', 'nextest', 'run', '--no-fail-fast',
               '-p', package, '-E', filterset]
        passed = gate_passes.command_pass(repo, label, cmd)
        test_legs.append((label, cmd, passed))
    pending_tests = [(label, cmd, p) for label, cmd, p in test_legs if not p.record]
    proof_passes = [gate_passes.proof_pass(repo, run) for run in plan.runs()] if run_gpu else []
    proof_cached = bool(proof_passes) and all(p.record for p in proof_passes)
    if proof_cached:
        # A saved timing red is not a pass: the proof gate reruns just those tests.
        import gpu_proofs_gate
        proof_cached = not gpu_proofs_gate.unmeasured_heavy(
            [row for passed, run in zip(proof_passes, plan.runs())
             for row in gpu_proofs_gate.receipt_timings(passed, run)])

    # Compile every test binary the hold will run before taking it, so the
    # hold covers test time only (BUG-w0hh (landing gate speed)). The legs
    # under the hold then find everything built and go straight to testing.
    print("[tests] " + cpu_plan.describe().replace("\n", "\n[tests] "), flush=True)
    # A leg whose compile failed is skipped by name (the build's red stands);
    # the hold still serves whatever else compiled.
    unbuilt = set()
    if pending_tests:
        builds = {}
        for _, cmd, _ in pending_tests:
            builds.setdefault(cmd[cmd.index('-p') + 1], []).append(cmd[cmd.index('-E') + 1])
        for package, filters in sorted(builds.items()):
            # Match each run's package selection. Building a union of packages
            # can unify extra dependency features and force a rebuild in the hold.
            label = 'tests-build' if len(builds) == 1 else f'tests-build/{package}'
            if build_leg(results, label, ['cargo', 'nextest', 'run', '--no-run',
                                         '-p', package, '-E', ' | '.join(filters)],
                         repo) == 'FAIL':
                unbuilt.add(package)
                if args.fail_fast:
                    return finish(repo, base_sha, results)
        for package in sorted(builds.keys() - unbuilt):
            command = ['cargo', 'nextest', 'list', '-p', package, '--message-format', 'json']
            code, out, err, duration = run_cmd(command, cwd=repo, timeout=600)
            try:
                if code:
                    raise ValueError(err or out)
                cpu_scope.validate_inventory(cpu_plan, package, json.loads(out))
            except (ValueError, KeyError, TypeError) as error:
                RAN_EVERY_CHECK.set(False)
                label = f'test-ownership/{package}'
                tail = [str(error), f'rerun: {rerun_command(command, repo)}']
                results.append(('FAIL', label, duration, tail))
                print_result(label, 'FAIL', duration, tail)
                unbuilt.add(package)
                if args.fail_fast:
                    return refuse(repo, base_sha, results)
        # Inventory can widen a path-derived module with no tests. Replace both
        # the command and its receipt key so the whole suite actually runs.
        selections = cpu_plan.selections()
        for index, (label, cmd, passed) in enumerate(test_legs):
            package = cmd[cmd.index('-p') + 1]
            if package not in unbuilt and cmd[cmd.index('-E') + 1] != selections[package]:
                cmd = [*cmd]
                cmd[cmd.index('-E') + 1] = selections[package]
                test_legs[index] = (label, cmd, gate_passes.command_pass(repo, label, cmd))
        for reason in sorted(cpu_plan.widening_reasons):
            print(f"[tests] {reason}", flush=True)
        pending_tests = [leg for leg in test_legs if not leg[2].record
                         and leg[1][leg[1].index('-p') + 1] not in unbuilt]
    elif test_legs:
        skip(results, 'tests-build', 'all selected tests already passed; no artifacts needed')
    else:
        skip(results, "tests-build", "no changed Rust modules or mapped integration binaries")
    if flows_pending:
        if build_leg(results, "flow-gate-build", [*flow_cmd, "--build-only"], repo) == "FAIL":
            unbuilt.add("flow-gate")
            if args.fail_fast:
                return finish(repo, base_sha, results)
    if run_gpu:
        gpu_args = [arg for path in root_gpu_paths for arg in ("--path", path)]
        if args.base != "origin/main":
            gpu_args += ["--base", base_sha]
        if proof_cached:
            skip(results, 'gpu-proofs-build', 'all selected proofs already passed; no artifacts needed')
        if not proof_cached and build_leg(results, "gpu-proofs-build",
                     ["python3", "scripts/gpu_proofs_gate.py", *gpu_args, "--build-only"],
                     repo) == "FAIL":
            unbuilt.add("gpu-proofs")
            if args.fail_fast:
                return finish(repo, base_sha, results)
    else:
        skip(results, "gpu-proofs-build", "no GPU paths touched" if not touches_gpu
             else args.skip_gpu or "GPU ownership planning failed")

    if unbuilt:
        RAN_EVERY_CHECK.set(False)

    planned_passes = [flow_pass, *(p for _, _, p in test_legs), *proof_passes]
    changed = gate_passes.changed_passes(planned_passes)
    if changed:
        tail = ['inputs changed after build planning: ' + ', '.join(changed),
                f'rerun: {rerun_command(readiness_cmd[:-1], repo)}']
        results.append(('FAIL', 'stable-inputs', None, tail))
        print_result('stable-inputs', 'FAIL', tail=tail)
        return refuse(repo, base_sha, results)

    # Nextest tests call GpuDevice::new_queued; each would queue behind every
    # agent's GPU run on its own. Hold the machine-wide GPU lock once, from
    # here through gpu-proofs: child test processes inherit an ancestor's hold,
    # so the landing waits once (visibly, on stdout) then runs straight through.
    # Keep the hold for scoped nextest: transitive helpers can open a device,
    # so source-path inspection alone cannot prove a selected test CPU-only.
    proofs_pending = run_gpu and not proof_cached and "gpu-proofs" not in unbuilt
    if expensive_blocked(args, results):
        return refuse(repo, base_sha, results)
    if pending_tests or (flows_pending and "flow-gate" not in unbuilt) or proofs_pending or gpu_tooling:
        print("[gpu-queue] taking the GPU lock for the flow-gate, tests and gpu-proofs legs", flush=True)
        started = time.monotonic()
        stack.enter_context(gpu_queue.hold("landing_gate flows+tests+gpu-proofs", out=sys.stdout))
        GPU_WAIT.set(time.monotonic() - started)
        print(f"[gpu-queue] held after {GPU_WAIT.get():.0f}s", flush=True)
        if gate_passes.changed_passes(planned_passes):
            tail = ['inputs changed while waiting for the GPU; replan before execution',
                    f'rerun: {rerun_command(readiness_cmd[:-1], repo)}']
            results.append(('FAIL', 'stable-inputs', None, tail))
            print_result('stable-inputs', 'FAIL', tail=tail)
            return refuse(repo, base_sha, results)

    # f. tests (if packages touched)
    if test_legs:
        for label, cmd, passed in test_legs:
            if cmd[cmd.index('-p') + 1] in unbuilt:
                skip(results, label, "tests-build failed")
                continue
            exit_, out, err, duration = run_check(label, cmd, cwd=repo, timeout=3600, passed=passed)
            tail = (out + err).rstrip().splitlines()[-20:]
            status = 'PASS' if exit_ == 0 else 'FAIL'
            results.append((status, label, duration, tail))
            print_result(label, status, duration, tail if exit_ else None)
            if status == 'FAIL' and args.fail_fast:
                return finish(repo, base_sha, results)
    else:
        skip(results, "tests", "no changed Rust modules or mapped integration binaries")

    if expensive_blocked(args, results):
        return refuse(repo, base_sha, results)

    if "flow-gate" in unbuilt:
        skip(results, "flow-gate", "flow-gate-build failed")
    elif flow_leg() == "FAIL" and args.fail_fast:
        return finish(repo, base_sha, results)

    # Nested-workspace proofs were compiled above and share this GPU hold.
    for check in gpu_tooling:
        exit_, out, err, duration = run_check(
            check["name"], check["argv"], cwd=repo,
            timeout=check.get("timeout", 120))
        tail = (out + err).rstrip().splitlines()[-20:]
        status = "PASS" if exit_ == 0 else "FAIL"
        results.append((status, check["name"], duration, tail))
        print_result(check["name"], status, duration, tail if exit_ else None)
        if exit_ and args.fail_fast:
            return finish(repo, base_sha, results)

    # g. gpu-proofs
    if touches_gpu:
        if args.skip_gpu:
            skip(results, "gpu-proofs", f"skipped by flag: {args.skip_gpu}")
        elif plan.unmapped:
            RAN_EVERY_CHECK.set(False)
            skip(results, "gpu-proofs", "GPU ownership planning failed")
        elif "gpu-proofs" in unbuilt:
            skip(results, "gpu-proofs", "gpu-proofs-build failed")
        else:
            print("[gpu-proofs] mode: scoped (focused tests + smoke; --all is nightly only)")
            print("[gpu-proofs] " + plan.describe().replace("\n", "\n[gpu-proofs] "), flush=True)
            cmd = ["python3", "scripts/gpu_proofs_gate.py", *gpu_args,
                   "--budget", str(gpu_scope.LANDING_BUDGET_S), "--learn-times"]
            # The GPU hold was taken before the tests leg and is still held.
            if proof_cached:
                for passed in proof_passes:
                    if not passed.reused():
                        results.append(('FAIL', 'stable-inputs', None,
                                        ['proof inputs changed before reuse']))
                        return refuse(repo, base_sha, results)
                import gpu_proofs_gate
                timings = [row for passed, run in zip(proof_passes, plan.runs())
                           for row in gpu_proofs_gate.receipt_timings(passed, run)]
                summary = io.StringIO()
                with contextlib.redirect_stdout(summary):
                    exit_ = gpu_proofs_gate.print_summary(
                        '', 0, timings, gpu_scope.LANDING_BUDGET_S, [], repo / 'Cargo.toml',
                        gpu_proofs_gate.unmeasured_heavy(timings),
                        gpu_proofs_gate.unknown_target_timings(timings))
                print(summary.getvalue(), end='', flush=True)
                if exit_:
                    RAN_EVERY_CHECK.set(False)
                deferred = [line for line in plan.describe().splitlines()
                            if line.startswith('GPU-PROOFS DEFERRED:')]
                for line in deferred:
                    print(line, flush=True)
                out = '\n'.join(['[REUSED] gpu-proofs', summary.getvalue(), *deferred])
                err, duration = '', 0.0
            else:
                exit_, out, err, duration = run_check("gpu-proofs", cmd, cwd=repo, timeout=7200)
                for line in out.splitlines():
                    if line.startswith(('[REUSED]', 'GPU-PROOFS BUDGET:', 'GPU-PROOFS DEFERRED:')):
                        print(line, flush=True)
            if exit_ == 0:
                if gate_passes.changed_passes(proof_passes):
                    RAN_EVERY_CHECK.set(False)
                    exit_, err = 1, err + '\nproof inputs changed during execution'
                else:
                    for passed in proof_passes:
                        passed.accepted()
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
            tail += [line for line in lines if line.startswith(('GPU-PROOFS DEFERRED:', 'rerun: '))
                     and line not in tail]
            status = "PASS" if exit_ == 0 else "FAIL"
            results.append((status, "gpu-proofs", duration, tail))
            print_result("gpu-proofs", status, duration, tail if exit_ != 0 else None)
            if status == "FAIL" and args.fail_fast:
                return finish(repo, base_sha, results)
    else:
        skip(results, "gpu-proofs", "no GPU paths touched")

    return finish(repo, base_sha, results)


def expensive_blocked(args, results):
    """A red cheap leg cannot spend flow/proof time without explicit diagnosis."""
    reds = [label for status, label, *_ in results if status == 'FAIL']
    if args.keep_going or not reds:
        return False
    RAN_EVERY_CHECK.set(False)
    reason = "cheap legs failed: " + ", ".join(reds)
    print("[INCOMPLETE] remaining device tests, flows and GPU proofs were not run because " + reason, flush=True)
    for label in ('tests', 'flow-gate', 'gpu-proofs'):
        skip(results, label, reason)
    return True


def refuse(repo, base_sha, results):
    """A stop before the checks could run: never the named-red exit."""
    RAN_EVERY_CHECK.set(False)
    return finish(repo, base_sha, results)


def finish(repo, base_sha, results):
    changed = gate_passes.changed_passes(gate_passes.accepted_passes())
    if changed:
        RAN_EVERY_CHECK.set(False)
        tail = ['inputs changed before verdict publication: ' + ', '.join(changed),
                'rerun: ' + rerun_command(['scripts/landing_gate.py', '--repo', str(repo),
                                           '--base', base_sha], repo)]
        results.append(('FAIL', 'stable-inputs', None, tail))
        print_result('stable-inputs', 'FAIL', tail=tail)
    if GATED_HEAD.get():
        current = run_cmd(['git', 'rev-parse', 'HEAD'], cwd=repo, timeout=30)
        dirty = run_cmd(['git', 'status', '--porcelain', '--untracked-files=normal'],
                        cwd=repo, timeout=30)
        if current[0] or dirty[0] or current[1].strip() != GATED_HEAD.get() or dirty[1].strip():
            results.append(('FAIL', 'stable-tree', None, ['tree changed during the gate; rerun to reuse unaffected passes']))
    results = [('REUSED' if status == 'PASS' and any(line.startswith('[REUSED]') for line in tail)
                else status, label, duration, tail) for status, label, duration, tail in results]
    # Summary
    passed = sum(1 for s, _, _, _ in results if s in {"PASS", "REUSED"})
    failed = sum(1 for s, _, _, _ in results if s == "FAIL")
    skipped = sum(1 for s, _, _, _ in results if s == "SKIP")
    for status, label, duration, tail in results:
        timeout = timeout_detail(tail)
        if timeout is not None:
            print(f"{status} {label} timed out at configured {timeout} seconds")
        elif duration:
            print(f"{status} {label} ({duration:.0f}s)")
        elif status == "SKIP" and tail:
            print(f"{status} {label} ({tail[0]})")
        else:
            print(f"{status} {label}")
        for line in tail:
            if line.startswith('GPU-PROOFS DEFERRED:') or (status == 'FAIL' and line.startswith('rerun: ')):
                print(line)
    for line in gate_passes.flaky_lines():
        print(line)
    if GPU_WAIT.get() is not None:
        print(f"gpu-queue wait: {GPU_WAIT.get():.0f}s")
    print(f"landing gate: {passed} passed, {failed} failed, {skipped} skipped")
    if failed:
        print("fix each red with its `rerun:` command above, then run the gate once more to land")

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
            slow_tests = SLOW_TESTS.get() or {}
            if label in slow_tests:
                check_entry["slow_tests"] = slow_tests[label]
            checks.append(check_entry)
        entry = {
            "ts": datetime.now(timezone.utc).isoformat(),
            "branch": branch,
            "checks": checks,
            "failed": failed,
            "gpu_wait_s": None if GPU_WAIT.get() is None else round(GPU_WAIT.get(), 1),
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
                        if gate_passes.changed_passes(gate_passes.accepted_passes()):
                            print('[FAIL] inputs changed before verdict publication')
                            print('rerun: ' + rerun_command(
                                ['scripts/landing_gate.py', '--repo', str(repo), '--base', base_sha], repo))
                            return 1
                        gate_runner.append_verdict(bead_id, verdict)
                        print(f"verdict stamped: {bead_id}")
        except Exception as e:
            print(f"[WARN] self-verdict failed: {e}")

    if failed == 0:
        print('[COMPLETE] landing gate: passed', flush=True)
        return 0
    moved = any(status == 'FAIL' and label == 'stable-tree' for status, label, _, _ in results)
    return CHECKS_RED if RAN_EVERY_CHECK.get() and not moved else 1


if __name__ == "__main__":
    sys.exit(main())
