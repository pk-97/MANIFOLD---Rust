#!/usr/bin/env python3
"""GPU-proofs landing gate wrapper — one consolidated drift report.

`cargo test --features gpu-proofs` alone stops at the
first failing test binary, so golden drift surfaces piecemeal over review
rounds. This wrapper streams output live, then parses the full captured run
into one summary: every failed test name, every golden-mismatch detail (file +
diff), and a per-binary pass/fail count. Every selected test binary runs to
completion with `--no-fail-fast`. Never nextest — process-per-test
defeats the GPU device lock. The test binaries are compiled first with no
lock held (`cargo test --no-run`, same arguments); then the test run holds the
machine-wide GPU queue (scripts/gpu_queue.py) and waits its turn behind any
other GPU run. `--build-only` stops after the compile. Landing nextest and
catalog checks use the same proof feature to reuse the renderer artifacts.
Builds disable incremental compilation so sccache can cache workspace crates.

Default mode is SCOPED: the branch's diff against `--base` (default
origin/main, plus uncommitted and untracked files) is mapped by
scripts/gpu_scope.py to the focused tests for what changed plus a fixed smoke
set. A touched GPU path with no mapping fails loudly; there is no silent
run-everything fallback. `--all` runs the whole suite (nightly trunk_health).
Explicit `--test NAME` / `--filter` / `--skip` bypass scoping for a hand-picked
run. `--budget SECONDS` reports a separate budget warning when passing tests
exceed it; test failures and hangs remain red. Scoped runs never drop tests
by duration. Successful gate-driven runs retain informational timings in the
Git common directory; only reviewed package/target/test entries committed in
scripts/gpu_test_times.json set watchdog allowances. `--record-times PATH`
exports successful measurements for review before landing a new heavy proof.

Scoped and explicit runs reuse shared content-addressed passes before building
or taking the GPU lock. --all and measurement requests always execute.
Each completed invocation saves its correctness pass and per-test times before
whole-leg policy checks. Reuse checks those times against current allowances
and budget, including the landing gate's fully cached path. Timing-table edits
change policy, not correctness receipts; source, selection, feature, dependency,
toolchain and execution-environment changes still invalidate affected passes.
Failures retry only failed invocations; passing siblings retain their receipts.

HANG WATCHDOG: the output is streamed and the one running test is timed. A test
that starts and does not finish (ok / FAILED / ignored) within its allowance
gets its process group killed and the gate fails with `GPU-PROOFS GATE: HUNG
<name> after Ns` (exit 4), so a hang cannot hold the machine-wide GPU lock.
Allowance = max(120s, 5x its time in scripts/gpu_test_times.json); a test with
no record gets 300s. `--hang-allowance SECONDS` replaces the 120s floor (and
the no-record 300s). A heartbeat naming the running test prints every 60s. A
hang is a red gate: never ignore the test, never skip it on rerun.

Exit 0 iff the underlying cargo build and run exited 0.

Obsolete when: cargo test reports cross-binary failure summaries natively
and the landing docs point at that instead.
"""

import argparse
import codecs
import contextlib
import fcntl
import functools
import json
import os
import queue
import re
import signal
import shlex
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

import gpu_queue
import gpu_scope
import diff_scope
import gate_passes
from gate_workspace import Workspace
from gate_cancellation import Cancelled, cancellation_signals

# Reserved for input changes; child failures with this status become ordinary reds.
INPUTS_CHANGED = 78

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
# Output written mid-test (native libraries print to the shared stream) splits
# "test X ... " from its result, which then arrives on a line of its own. The
# time still belongs to X, not to the next test that finishes on one line.
TEST_START_RE = re.compile(r"^test (\S+) \.\.\. ")
BARE_RESULT_RE = re.compile(r"^(ok|FAILED|ignored)\s*$")
IGNORED_LINE_RE = re.compile(r"^test (\S+) \.\.\. ignored\b")

# Hang watchdog: a test that started and never finished holds the machine-wide
# GPU lock for everyone. Allowance = max(floor, multiple x recorded time); a
# test with no record gets the no-record allowance.
HANG_FLOOR_S = 120.0
HANG_MULTIPLE = 5.0
NO_RECORD_ALLOWANCE_S = 300.0
MAX_HANG_ALLOWANCE_S = 3600.0
HEARTBEAT_S = 60.0
WATCH_TICK_S = 1.0

FAILURES_BLOCK_RE = re.compile(r"failures:\n((?:    \S.*\n)+)\ntest result:")


def default_manifest_path() -> Path:
    return Path(__file__).resolve().parent.parent / "Cargo.toml"


def cargo_test_cmd(
    manifest_path: Path,
    targets: list[str] | None = None,
    full_suite: bool = False,
    lib: bool = False,
    package: str | None = None,
) -> list[str]:
    """The `cargo test` command up to the libtest `--`, shared by the build
    and the run so the run finds every binary already built."""
    if full_suite and targets is not None:
        raise ValueError("full_suite and targets are mutually exclusive")
    cmd = [
        "cargo",
        "test",
        "--features",
        "gpu-proofs",
        "--no-fail-fast",
        "--manifest-path",
        str(manifest_path),
    ]
    if package is not None:
        cmd[2:2] = ["-p", package]
    if not full_suite:
        if lib:
            cmd.append("--lib")
        for target in targets or ([] if lib else ["gpu_proofs"]):
            cmd.extend(["--test", target])
    return cmd


def build_environment():
    environment = os.environ.copy()
    environment["CARGO_INCREMENTAL"] = "0"
    return environment


def build_tests(manifest_path: Path, runs: list[dict]) -> int:
    """Compile every run's test binaries with no GPU lock held, so the hold
    covers test time only: the run that follows re-checks fingerprints and
    starts testing. Returns the first nonzero cargo exit, else 0."""
    built: list[list[str]] = []
    for run in runs:
        package = run.get("package")
        if not package:
            raise ValueError("GPU proof run has no Cargo package owner")
        cmd = cargo_test_cmd(manifest_path, run["targets"], run["full"], run["lib"],
                             package) + ["--no-run"]
        if cmd in built:
            continue
        built.append(cmd)
        print(f"$ {' '.join(cmd)}", flush=True)
        code = subprocess.run(cmd, env=build_environment()).returncode
        if code:
            return code
    return 0


def target_from_binary(label: str, target_specs: list[dict]) -> str | None:
    """Map Cargo's `Running` label to a metadata target without guessing."""
    path = label.split(" (", 1)[0].strip()
    for spec in target_specs:
        source = spec.get("src_path")
        if not source:
            continue
        source_path = Path(source)
        parts = source_path.parts
        anchor = next((i for i, part in enumerate(parts)
                       if part in {"src", "tests", "examples", "benches"}), None)
        if anchor is None:
            continue
        suffix = "/".join(parts[anchor:])
        if path.endswith(suffix):
            return "lib" if "lib" in spec.get("kind", []) else spec["name"]
    return None


def cancellable_proof(function):
    @functools.wraps(function)
    def run(*args, **kwargs):
        with cancellation_signals():
            return function(*args, **kwargs)
    return run


@cancellable_proof
def run_gate(
    manifest_path: Path,
    filters: list[str],
    skips: list[str],
    targets: list[str] | None = None,
    full_suite: bool = False,
    lib: bool = False,
    timings: list | None = None,
    hung: list | None = None,
    hang_floor: float | None = None,
    package: str | None = None,
    target: str | None = None,
    budgeted: bool = True,
    target_specs: list[dict] | None = None,
) -> tuple[int, str]:
    cmd = cargo_test_cmd(manifest_path, targets, full_suite, lib, package)
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

    # Own process group so a hang kill takes cargo and the test binary, and
    # nothing else.
    # Learned timings are advisory until reviewed into the committed table.
    resolver = ((lambda label: target_from_binary(label, target_specs))
                if target_specs is not None else None)
    watchdog = Watchdog(gpu_scope.read_times(gpu_scope.TIMES_PATH), hang_floor,
                        package=package, target=target, target_resolver=resolver)
    chunks: queue.Queue = queue.Queue()

    def pump() -> None:
        for chunk in _chunks(proc.stdout):
            chunks.put(chunk)
        chunks.put(None)

    # Defer cancellation across spawn/registration so the handler can never
    # unwind without a handle to the newly created Cargo process group.
    blocked = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
    proc = None
    pump_thread = None
    lines: list[str] = []
    state: dict = {"t": None, "bin": ""}
    pending = ""
    try:
        proc = subprocess.Popen(
            cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            bufsize=0, start_new_session=True, env=build_environment(),
        )
        assert proc.stdout is not None
        pump_thread = threading.Thread(target=pump, daemon=True)
        pump_thread.start()
        signal.pthread_sigmask(signal.SIG_SETMASK, blocked)
        while True:
            try:
                chunk = chunks.get(timeout=WATCH_TICK_S)
            except queue.Empty:
                chunk = ""
            now = time.monotonic()
            if chunk is None:
                break
            # Publish immediately, including libtest's unfinished status line.
            # Parsing still consumes complete lines below; watchers must not
            # wait for the rest of the test binary before seeing a failure.
            if chunk:
                print(chunk, end="", flush=True)
            pending += chunk
            while "\n" in pending:
                line, pending = pending.split("\n", 1)
                line += "\n"
                lines.append(line)
                if timings is not None:
                    record_timing(line, now, state, timings, package=package,
                                  target=target, budgeted=budgeted,
                                  target_resolver=resolver)
                watchdog.feed_line(line, now)
            watchdog.feed_partial(pending, now)
            beat = watchdog.heartbeat(now)
            if beat:
                print(('\n' if pending else '') + beat, flush=True)
            verdict = watchdog.check(now)
            if verdict:
                name, waited, allowance = verdict
                print(('\n' if pending else '') + f"GPU-PROOFS GATE: HUNG {name} after {waited:.0f}s "
                      f"(allowance {allowance:.0f}s; killing the test process group)", flush=True)
                _kill_group(proc)
                if hung is not None:
                    hung.append((name, waited))
                break
        if pending:
            lines.append(pending)
        exit_code = proc.wait()
        pump_thread.join()
    except BaseException:
        if proc is not None:
            _kill_group(proc)
            proc.wait()
            # EOF confirms that Cargo's descendants released the inherited
            # pipe. Do not let our parent release GPU ownership before this.
            if pump_thread is not None:
                pump_thread.join()
        raise
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, blocked)
        if proc is not None and hasattr(proc.stdout, 'close'):
            proc.stdout.close()
    return exit_code, "".join(lines)


def _chunks(stream):
    """Yield decoded text as it arrives, including a line's unfinished tail."""
    if not hasattr(stream, "read"):
        yield from stream
        return
    decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
    while True:
        # BufferedReader.read(size) can wait for size bytes or EOF. read1
        # returns the currently available pipe data, like unbuffered FileIO.
        data = (stream.read1 if hasattr(stream, "read1") else stream.read)(65536)
        if not data:
            tail = decoder.decode(b"", final=True)
            if tail:
                yield tail
            break
        yield decoder.decode(data)


def _kill_group(proc) -> None:
    """SIGKILL the process group this gate started; never anything else."""
    with contextlib.suppress(ProcessLookupError):
        os.killpg(proc.pid, signal.SIGKILL)


def timing_key(package: str | None, target: str | None, test: str) -> str:
    """Stable identity for a proof allowance, including its Cargo owner."""
    if package is None:
        return test
    return "/".join((package or "?", target or "?", test))


def timing_fields(entry):
    """Return (package, target, test, seconds, status, budgeted) for old/new rows."""
    if isinstance(entry, dict):
        return (entry.get("package"), entry.get("target"), entry["test"],
                entry["seconds"], entry["status"], entry.get("budgeted", True))
    if len(entry) == 6:
        return entry
    if len(entry) == 5:
        name, seconds, binary, budgeted, status = entry
        return None, binary, name, seconds, status, budgeted
    name, seconds, binary, status = entry
    return None, binary, name, seconds, status, True


def timing_entry(package, target, test, seconds, status, budgeted):
    return {"package": package, "target": target, "test": test,
            "seconds": seconds, "status": status, "budgeted": budgeted}


class Watchdog:
    """Tracks the one running test and says when it has outlived its allowance.

    Libtest prints `test X ... ` with no newline before it runs, so a hang shows
    up as an unfinished tail, not a line; `feed_partial` sees that tail. Pure
    logic over (text, now): no clock, no process, unit-testable without a GPU.
    """

    def __init__(self, times: dict, floor: float | None = None, *,
                 package: str | None = None, target: str | None = None,
                 target_resolver=None):
        self.times = times
        self.floor = HANG_FLOOR_S if floor is None else floor
        self.no_record = NO_RECORD_ALLOWANCE_S if floor is None else floor
        self.package = package
        self.target = target
        self.target_resolver = target_resolver
        self.name: str | None = None
        self.started = 0.0
        self.last_beat = 0.0

    def allowance(self, name: str) -> float:
        rec = self.times.get(timing_key(self.package, self.target, name))
        if rec is None and self.package is None and self.target is None:
            # Legacy unit callers can still inspect old name-only tables. Real
            # runs always carry package/target and must not inherit collisions.
            rec = self.times.get(name)
        if isinstance(rec, dict):
            rec = rec.get("s", rec.get("seconds"))
        if rec is None:
            return self.no_record
        return max(self.floor, HANG_MULTIPLE * rec)

    def _start(self, name: str, now: float) -> None:
        if self.name != name:
            self.name, self.started, self.last_beat = name, now, now

    def feed_line(self, line: str, now: float) -> None:
        binary = RUNNING_BINARY_RE.match(line)
        if binary:
            if self.target_resolver is not None:
                self.target = self.target_resolver(binary.group(1))
            self.name = None
            return
        m = TEST_LINE_RE.match(line) or IGNORED_LINE_RE.match(line)
        if m:
            self.name = None
            return
        if BARE_RESULT_RE.match(line):
            self.name = None
            return
        m = TEST_START_RE.match(line)
        if m:
            self._start(m.group(1), now)

    def feed_partial(self, tail: str, now: float) -> None:
        m = TEST_START_RE.match(tail)
        if m:
            self._start(m.group(1), now)

    def check(self, now: float):
        """(name, waited, allowance) once the running test is over its allowance."""
        if self.name is None:
            return None
        waited = now - self.started
        allowance = self.allowance(self.name)
        return (self.name, waited, allowance) if waited > allowance else None

    def heartbeat(self, now: float) -> str | None:
        if self.name is None or now - self.last_beat < HEARTBEAT_S:
            return None
        self.last_beat = now
        return (f"[gate] still running: {self.name} for {now - self.started:.0f}s "
                f"(hang allowance {self.allowance(self.name):.0f}s)")


def record_timing(line: str, now: float, state: dict, timings: list, *,
                  package: str | None = None, target: str | None = None,
                  budgeted: bool = True, target_resolver=None) -> None:
    """Append a completed test timing with its package/target identity."""
    m = RUNNING_BINARY_RE.match(line)
    if m:
        state["t"], state["bin"] = now, m.group(1)
        state["target"] = (target if target not in (None, "all") else
                            (target_resolver(m.group(1)) if target_resolver else None))
        return
    if state.get("t") is None:
        return
    m = TEST_LINE_RE.match(line)
    name = m.group(1) if m else None
    if name is None and state.get("open") and BARE_RESULT_RE.match(line):
        name = state["open"]
    if name is not None:
        status = m.group(2) if m else BARE_RESULT_RE.match(line).group(1)
        row_target = target if target not in (None, "all") else state.get("target")
        if row_target is None and target_resolver is None:
            row_target = state.get("bin")
        if row_target is None:
            row_target = "unknown"
        if package is None:
            # Unit callers and old queue integrations still receive the former
            # tuple shape; all real gate runs provide package ownership.
            timings.append((name, now - state["t"], row_target, status))
        else:
            timings.append(timing_entry(package, row_target, name,
                                        now - state["t"], status, budgeted))
        state["t"], state["open"] = now, None
        return
    m = TEST_START_RE.match(line)
    if m:
        state["open"] = m.group(1)


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
    return sorted(timings, key=lambda t: timing_fields(t)[3], reverse=True)[:n]


def budgeted_seconds(timings: list) -> float:
    return sum(seconds for _package, _target, _name, seconds, _status, budgeted
               in (timing_fields(t) for t in timings) if budgeted)


def write_timings_md(path: Path, timings: list, n: int = 25) -> None:
    rows = ["# Slowest GPU tests", "", f"{len(timings)} tests, "
            f"{sum(timing_fields(t)[3] for t in timings):.0f}s total test time.", "",
            "| # | seconds | test | binary |", "|---|---|---|---|"]
    for i, entry in enumerate(slowest(timings, n), 1):
        _package, target, name, secs, _status, _budgeted = timing_fields(entry)
        rows.append(f"| {i} | {secs:.1f} | `{name}` | {target} |")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(rows) + "\n")


def write_times_json(path: Path, timings: list, *, merge=False, learned=False) -> str:
    """Write successful allowances keyed by package, target, and test."""
    if any(timing_fields(entry)[4] == "FAILED" for entry in timings):
        return "GPU test times not written: a failed proof cannot export allowances"
    old = gpu_scope.read_times(gpu_scope.TIMES_PATH)
    new = {}
    for entry in timings:
        package, target, name, seconds, status, _budgeted = timing_fields(entry)
        if status == "ok":
            if package is not None:
                # Drop a pre-metadata bare-name seed when replacing it with the
                # scoped identity; otherwise merge would retain two allowances.
                new.pop(name, None)
            new[timing_key(package, target, name)] = round(seconds, 1)
    if merge and not learned:
        new = gpu_scope.merge_times(old, gpu_scope.read_times(path), new)
        for entry in timings:
            _package, _target, name, _seconds, status, _budgeted = timing_fields(entry)
            if status == "ok" and _package is not None:
                new.pop(name, None)
            if status != "ok":
                new.pop(timing_key(_package, _target, name), None)
                new.pop(name, None)
    sha = subprocess.run(["git", "-C", str(Path(__file__).resolve().parent), "rev-parse", "HEAD"],
                         capture_output=True, text=True).stdout.strip()
    entries = new
    if learned:
        entries = json.loads(path.read_text())["tests"] if path.exists() else {}
        entries.update({n: {"s": secs, "sha": sha, "at": time.time()}
                        for n, secs in new.items()})
    path.parent.mkdir(parents=True, exist_ok=True)
    # Readers in another slot must see either complete version, never a
    # partially written JSON file.
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as out:
        temporary = Path(out.name)
        try:
            out.write(json.dumps(
                {"measured_at": time.strftime("%Y-%m-%d"), "sha": sha,
                 "tests": dict(sorted(entries.items()))}, indent=2) + "\n")
            out.close()
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)
    thr = gpu_scope.SLOW_THRESHOLD_S
    lines = [f"GPU test times written to {path} (threshold {thr}s)"]
    for n in sorted(set(old) | set(new)):
        o, c = old.get(n), new.get(n)
        if c is None and o > thr:
            lines.append(f"  gone: {n} (was {o:.0f}s)")
        elif o is None and c > thr:
            lines.append(f"  new SLOW: {n} {c:.0f}s")
        elif o is not None and c is not None:
            if (o > thr) != (c > thr):
                lines.append(f"  {'now SLOW' if c > thr else 'now fast'}: {n} {o:.0f}s -> {c:.0f}s")
            elif c > thr and abs(c - o) > 0.25 * o:
                lines.append(f"  moved: {n} {o:.0f}s -> {c:.0f}s")
    if len(lines) == 1:
        lines.append("  no threshold crossings vs the committed file")
    lines.append("To adopt: review, then commit this file as scripts/gpu_test_times.json on a branch.")
    return "\n".join(lines)


def receipt_timings(passed, run):
    """Restore measurements with the current selection's budget policy."""
    if 'timings' in passed.record:
        return [timing_entry(package, target, name, seconds, status, run['budgeted'])
                for package, target, name, seconds, status, _ in
                (timing_fields(row) for row in passed.record['timings'])]
    # Legacy receipts passed policy before publication; raw queue receipts
    # cannot exceed the slow threshold. Neither contains per-test measurements.
    target = ','.join(run['targets'] or []) or ('lib' if run['lib'] else 'all')
    return [timing_entry(run.get('package'), target, 'reused proof set',
                         passed.record['seconds'], 'ok', run['budgeted'])]


def unmeasured_heavy(timings: list) -> list[dict]:
    """Find successful slow tests absent from the reviewed committed table."""
    reviewed = gpu_scope.read_times(gpu_scope.TIMES_PATH)
    findings = []
    for entry in timings:
        package, target, name, seconds, status, _budgeted = timing_fields(entry)
        key = timing_key(package, target, name)
        if (status == "ok" and package is not None and target != "unknown"
                and name != 'reused proof set'
                and seconds > gpu_scope.SLOW_THRESHOLD_S
                and key not in reviewed):
            findings.append({"package": package, "target": target, "test": name,
                             "seconds": seconds, "key": key})
    return findings


def unknown_target_timings(timings: list) -> list[dict]:
    return [{"package": package, "target": target, "test": name,
             "seconds": seconds, "key": timing_key(package, target, name)}
            for package, target, name, seconds, status, _budgeted
            in (timing_fields(entry) for entry in timings)
            if status == "ok" and package is not None and target == "unknown"]


def measurement_command(finding: dict, manifest_path: Path) -> str:
    command = [str(Path(__file__).resolve()), "--manifest-path", str(manifest_path),
               "--package", finding["package"]]
    if finding["target"] != "lib":
        command.extend(["--test", finding["target"]])
    command.extend(["--filter", finding["test"], "--record-times",
                    "/tmp/gpu_test_times.measure.json", "--hang-allowance",
                    str(int(NO_RECORD_ALLOWANCE_S))])
    return shlex.join(command)


def failure_rerun_commands(failed_tests: list[str], timings: list,
                           manifest_path: Path | None = None) -> tuple[list[str], list[str]]:
    """Build owner-qualified reruns from the identities observed by Cargo.

    A proof target name is not a package key: more than one gpu-proofs package
    may expose a target such as ``gpu_proofs``.  The normalized timing rows
    retain the package/target pair from the run that produced each failure, so
    use that identity instead of asking Cargo to resolve a bare target again.
    """
    candidates = []
    for entry in timings:
        package, target, test, _seconds, status, _budgeted = timing_fields(entry)
        if (test in failed_tests and status == "FAILED" and package
                and target and target != "unknown"):
            candidates.append((test, package, target))

    commands = []
    unresolved = []
    used = set()
    gate = Path(__file__).resolve()
    manifest = ["--manifest-path", str(manifest_path)] if manifest_path else []
    for name in failed_tests:
        match = next(((index, candidate) for index, candidate in enumerate(candidates)
                      if index not in used and candidate[0] == name), None)
        if match is None:
            # A failure normally has a timing row.  Keep the report honest if
            # Cargo omitted its result line: do not print a rerun that would
            # fall back to ambiguous target ownership.
            unresolved.append(name)
            continue
        index, (_test, package, target) = match
        used.add(index)
        command = [str(gate), *manifest, "--package", package]
        if target != "lib":
            command.extend(["--test", target])
        command.extend(["--filter", name])
        commands.append(shlex.join(command))
    return commands, unresolved


def remember_times(timings: list, exit_code: int, hung: list) -> None:
    """Learn only from a completed passing invocation, never a failure or hang."""
    if exit_code or hung or not timings:
        return
    path = gpu_scope.learned_times_path()
    if path is None:
        print("[WARN] GPU timings not retained: cannot resolve the Git common directory")
        return
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        # Separate from the GPU lock; protects partial measurement merges.
        with path.with_suffix(".lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            write_times_json(path, timings, learned=True)
        print(f"[gpu-times] retained {len(timings)} informational measurements in {path}; "
              "watchdog allowances require committed package/target/test entries")
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"[WARN] GPU timings not retained: {error}")


def print_summary(
    output: str,
    exit_code: int,
    timings: list | None = None,
    budget: float | None = None,
    hung: list | None = None,
    manifest_path: Path | None = None,
    unmeasured: list[dict] | None = None,
    unknown_targets: list[dict] | None = None,
) -> int:
    """Print the consolidated report; return the final exit code."""
    failed_tests = parse_failed_tests(output)
    goldens = parse_golden_mismatches(output)
    binaries = parse_binaries(output)
    timings = timings or []
    unmeasured = unmeasured or []
    unknown_targets = unknown_targets or []
    spent = budgeted_seconds(timings)
    over_budget = budget is not None and spent > budget

    print("\n" + "=" * 72)
    print("GPU-PROOFS GATE SUMMARY")
    print("=" * 72)

    if failed_tests:
        print(f"\nFailed tests ({len(failed_tests)}):")
        for name in failed_tests:
            print(f"  - {name}")
        # One focused run per failure, queued and recorded like the gate.
        reruns, unresolved = failure_rerun_commands(failed_tests, timings, manifest_path)
        for command in reruns:
            print(f"rerun: {command}")
        for name in unresolved:
            print(f"rerun unavailable: {name} (failed test has no package/target identity)")
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
        for entry in slowest(timings, 10):
            package, target, name, secs, _status, _budgeted = timing_fields(entry)
            owner = f"{package}/" if package else ""
            print(f"  - {secs:7.1f}s {owner}{target}/{name}")

    if binaries:
        print("\nPer-binary results:")
        for label, status, passed, failed in binaries:
            print(f"  - {label}: {status} ({passed} passed, {failed} failed)")
    else:
        print("\nPer-binary results: none parsed")

    print()
    if hung:
        for name, waited in hung:
            print(f"GPU-PROOFS GATE: HUNG {name} after {waited:.0f}s")
        print("GPU-PROOFS GATE: FAIL (hung test killed; a hang is a red gate, never skip or ignore it)")
        return 4
    if unmeasured:
        print("GPU-PROOFS TIMING: FAIL (new heavy proof lacks a reviewed allowance)")
        for finding in unmeasured:
            print(f"  - {finding['key']}: {finding['seconds']:.1f}s")
            print(f"    measure: {measurement_command(finding, manifest_path or default_manifest_path())}")
        return 5
    if unknown_targets:
        print("GPU-PROOFS TIMING: FAIL (Cargo emitted an unmapped proof target)")
        for finding in unknown_targets:
            print(f"  - {finding['package']}/{finding['test']}: target path was not in Cargo metadata")
        return 6
    # Output evidence also wins over an erroneously successful cargo status.
    if failed_tests or goldens or any(status == "FAILED" for _, status, _, _ in binaries):
        exit_code = exit_code or 1
    if over_budget:
        print(f"GPU-PROOFS BUDGET: OVER ({spent:.0f}s > {budget:.0f}s; "
              "inspect the slowest tests above; successful gate-driven runs retain timings automatically. "
              "Shorten or narrow the remaining fast proofs; do not raise the budget)")
    if exit_code == 0:
        print("GPU-PROOFS GATE: PASS")
    elif not failed_tests and not goldens:
        print(f"GPU-PROOFS GATE: FAIL (cargo exit {exit_code}, no test failure parsed — not the budget)")
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
    paths = set(diff_scope.effective_paths(repo, mb.stdout.strip(), head=None)[0])
    for path in git_lines(repo, "ls-files", "--others", "--exclude-standard", "-z"):
        suffix = Path(path).suffix
        if suffix in {".md", ".txt"}:
            continue
        if suffix in {".rs", ".wgsl", ".py"} and not any(
                s.strip() for s in diff_scope.code_lines((repo / path).read_text(), suffix)):
            continue
        paths.add(path)
    return sorted(paths)


def default_gpu_package(workspace: Workspace) -> str:
    """Resolve the package owning the conventional GPU proof test target."""
    return workspace.binary_owner("gpu_proofs")


def package_for_run(workspace: Workspace, run: dict) -> str:
    package = run.get("package")
    targets = run.get("targets") or []
    if package is not None:
        if package not in workspace.feature_packages("gpu-proofs"):
            raise ValueError(f"package {package} has no gpu-proofs feature")
        owned_targets = {target["name"] for target in workspace.targets(package, "test")}
        unknown = sorted(set(targets) - owned_targets)
        if unknown:
            raise ValueError(f"targets {unknown} are not owned by {package}")
        return package
    owners = {workspace.binary_owner(target) for target in targets}
    if len(owners) == 1:
        return next(iter(owners))
    if len(owners) > 1:
        raise ValueError(f"targets have multiple owners: {sorted(owners)}")
    return default_gpu_package(workspace)


def normalize_runs(workspace: Workspace, runs: list[dict]) -> list[dict]:
    """Return the canonical one-package/one-target proof invocation shape."""
    normalized = []
    for source in runs:
        package = package_for_run(workspace, source)
        targets = list(source.get("targets") or [])
        if len(targets) > 1:
            for target in targets:
                child = dict(source, package=package, targets=[target], lib=False,
                             full=False, target=target)
                normalized.extend(normalize_runs(workspace, [child]))
            continue
        if source.get("lib"):
            target = "lib"
            cargo_targets = []
        elif targets:
            target = source.get("target") or targets[0]
            cargo_targets = [targets[0]]
        elif source.get("full"):
            normalized.append(dict(source, package=package, targets=None,
                                   target=None, full=True))
            continue
        else:
            target = source.get("target") or "gpu_proofs"
            cargo_targets = [target]
        normalized.append(dict(source, package=package, targets=cargo_targets,
                               target=target, full=False))
    return normalized


def all_runs(workspace: Workspace) -> list[dict]:
    """Run each gpu-proofs package's complete Cargo test target set."""
    runs = []
    for package in workspace.feature_packages("gpu-proofs"):
        target_specs = workspace.targets(package)
        if not target_specs:
            raise ValueError(f"package {package} has gpu-proofs but no targets")
        runs.append({"package": package, "targets": None, "lib": False,
                     "filters": [], "skips": [], "budgeted": False, "full": True,
                     "target": None, "target_specs": target_specs})
    return runs


def main() -> int:
    try:
        with cancellation_signals(), gate_passes.session():
            return _main()
    except Cancelled as error:
        print(f'[INCOMPLETE] GPU proofs cancelled by {error}; descendants stopped', flush=True)
        return 128 + error.signum


def _main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest-path",
        type=Path,
        default=None,
        help="Path to the workspace Cargo.toml (default: repo root next to scripts/)",
    )
    parser.add_argument("--package", default=None,
                        help="Cargo package to own an explicit proof run")
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
                        help="warn separately if passing tests exceed this (landing passes "
                        f"{gpu_scope.LANDING_BUDGET_S})")
    parser.add_argument("--hang-allowance", type=float, default=None, metavar="SECONDS",
                        help="floor of the per-test hang allowance (default "
                        f"{HANG_FLOOR_S:.0f}s; also the allowance of a test with no recorded time, "
                        f"which otherwise gets {NO_RECORD_ALLOWANCE_S:.0f}s)")
    parser.add_argument("--build-only", action="store_true",
                        help="compile the selected test binaries and stop (no GPU lock); "
                        "the landing gate runs this before taking its hold")
    parser.add_argument("--timings-md", type=Path, default=None,
                        help="write the 25 slowest tests as markdown to this path")
    parser.add_argument("--record-times", type=Path, default=None, metavar="PATH",
                        help="merge measured per-test seconds into PATH and print the diff "
                        "vs scripts/gpu_test_times.json (use with --all)")
    parser.add_argument("--learn-times", action="store_true",
                        help="retain passing gate-driven measurements in the shared cache")
    parser.add_argument("--forget", metavar="NAME", help="remove a shared timing entry and exit")
    args = parser.parse_args()
    if args.hang_allowance is not None and not 0 < args.hang_allowance <= MAX_HANG_ALLOWANCE_S:
        parser.error(f"--hang-allowance must be > 0 and <= {MAX_HANG_ALLOWANCE_S:.0f}s")
    if args.forget:
        path = gpu_scope.learned_times_path()
        if path is None:
            print("Cannot resolve shared GPU timing cache")
            return 2
        with path.with_suffix(".lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if path.exists():
                data = json.loads(path.read_text())
                data["tests"].pop(args.forget, None)
                with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as out:
                    json.dump(data, out)
                Path(out.name).replace(path)
        print(f"Forgot shared GPU timing: {args.forget}")
        return 0

    manifest_path = args.manifest_path or default_manifest_path()
    repo = manifest_path.parent
    try:
        workspace = Workspace(repo)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f"GPU-PROOFS METADATA: FAIL - {error}")
        return 2
    explicit = bool(args.filter or args.skip or args.targets or args.package)

    if args.all_tests:
        print("GPU-PROOFS MODE: all (--all: every test binary, no scoping)", flush=True)
        try:
            runs = all_runs(workspace)
        except (OSError, ValueError, KeyError, TypeError) as error:
            print(f"GPU-PROOFS SCOPE: FAIL - {error}")
            return 2
        for run in runs:
            run["filters"], run["skips"] = args.filter, args.skip
    elif explicit:
        print("GPU-PROOFS MODE: explicit (--test/--filter/--skip given; no scoping)", flush=True)
        package_lib = args.package is not None and args.targets is None
        runs = [{"package": args.package,
                 "targets": [] if package_lib else (args.targets or ["gpu_proofs"]),
                 "lib": package_lib, "filters": args.filter, "skips": args.skip,
                 "budgeted": True, "full": False}]
    else:
        try:
            paths = args.path if args.path is not None else changed_paths(repo, args.base)
            plan = gpu_scope.plan_for_paths(paths, repo, base=args.base, workspace=workspace)
        except RuntimeError as error:
            print(f"GPU-PROOFS SCOPE: FAIL - {error}")
            return 2
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

    try:
        runs = normalize_runs(workspace, runs)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"GPU-PROOFS SCOPE: FAIL - {error}")
        return 2

    # Nightly/full sweeps and measurement requests always execute. The key is
    # per cargo invocation, so queue-wrapped standalone runs count too.
    reuse = not (args.all_tests or args.record_times or args.timings_md or args.hang_allowance)
    passes = [gate_passes.proof_pass(repo, run) if reuse else None for run in runs]
    pending = [run for run, p in zip(runs, passes) if not (p and p.record)]
    build_code = build_tests(manifest_path, pending) if pending else 0
    if build_code:
        print(f"GPU-PROOFS GATE: FAIL (test build failed, exit {build_code}; no GPU lock taken)")
        return 1 if build_code == INPUTS_CHANGED else build_code
    if args.build_only:
        print("GPU-PROOFS GATE: BUILT (--build-only; no test run, no GPU lock taken)")
        return 0

    if gate_passes.changed_passes(passes):
        print('GPU-PROOFS GATE: FAIL (inputs changed after planning; rerun before GPU admission)')
        return INPUTS_CHANGED
    exit_code, outputs, all_timings, hung = 0, [], [], []
    # One GPU run on the machine at a time (scripts/gpu_queue.py). Held for all
    # cargo runs so another run cannot interleave between test binaries.
    recorded_timings = []
    with gpu_queue.hold("gpu_proofs_gate") if pending else contextlib.nullcontext():
        if gate_passes.changed_passes(passes):
            print('GPU-PROOFS GATE: FAIL (inputs changed while waiting for the GPU)')
            return INPUTS_CHANGED
        for run, passed in zip(runs, passes):
            if passed and passed.record:
                if not passed.reused():
                    print('GPU-PROOFS GATE: FAIL (inputs changed before reuse)')
                    return INPUTS_CHANGED
                all_timings.extend(receipt_timings(passed, run))
                continue
            run_timings: list = []
            code, output = run_gate(manifest_path, run["filters"], run["skips"], run["targets"],
                                    run["full"], run["lib"], run_timings, hung,
                                    args.hang_allowance, run["package"], run["target"],
                                    run["budgeted"], run.get("target_specs"))
            if code == INPUTS_CHANGED:
                code = 1
            if (hung or parse_failed_tests(output) or parse_golden_mismatches(output)
                    or any(status == "FAILED" for _, status, _, _ in parse_binaries(output))
                    or any(timing_fields(t)[4] == "FAILED" for t in run_timings)):
                code = code or 1
            exit_code = exit_code or code
            outputs.append(output)
            normalized = []
            for entry in run_timings:
                row_package, row_target, name, seconds, status, _row_budgeted = timing_fields(entry)
                normalized.append(timing_entry(run.get("package") or row_package,
                                               run.get("target") or row_target,
                                               name, seconds, status, run["budgeted"]))
            # Publish each run before another run or whole-leg policy can fail.
            if passed and passed.save(code, sum(timing_fields(t)[3] for t in normalized),
                                      timings=normalized) is False:
                print('GPU-PROOFS GATE: FAIL (inputs changed before receipt publication)')
                return INPUTS_CHANGED
            all_timings += normalized
            recorded = list(normalized)
            recorded_timings.extend(recorded)
            if hung:
                break
    output = "".join(outputs)
    if args.timings_md:
        write_timings_md(args.timings_md, all_timings)
    functional_ok = (exit_code == 0 and not hung and not parse_failed_tests(output)
                     and not parse_golden_mismatches(output)
                     and not any(status == "FAILED" for _, status, _, _ in parse_binaries(output))
                     and not any(timing_fields(entry)[4] == "FAILED"
                                 for entry in recorded_timings))
    if args.record_times and functional_ok:
        print(write_times_json(args.record_times, recorded_timings, merge=True))
    timing_red = [] if args.all_tests or args.record_times else unmeasured_heavy(all_timings)
    unknown_red = unknown_target_timings(all_timings)
    verdict = print_summary(output, exit_code, all_timings, args.budget, hung, manifest_path,
                            timing_red, unknown_red)
    if args.learn_times:
        remember_times(recorded_timings, verdict, hung)
    if gate_passes.changed_passes([p for p in passes if p]):
        print('GPU-PROOFS GATE: FAIL (inputs changed before receipt publication)')
        return INPUTS_CHANGED
    return verdict


if __name__ == "__main__":
    sys.exit(main())
