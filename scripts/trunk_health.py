#!/usr/bin/env python3
"""Nightly trunk-health sweep — the workspace-wide checks that used to run at every landing.

Moved out per GIT_TREE_DISCIPLINE.md section 2 (Landing protocol), 2026-07-29.
Runs in the main checkout against current main; a red gate files a P1 trunk-health bead.
Scheduled by launchd (scripts/com.manifold.trunk-health.plist).
"""


import argparse
import contextlib
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path
import shutil

MAIN_CHECKOUT = Path("/Users/peterkiemann/MANIFOLD - Rust")
LOG_DIR = MAIN_CHECKOUT / ".claude/orchestration/trunk-health"
TOOL_DIRS = [os.path.expanduser("~/.cargo/bin"), "/opt/homebrew/bin", "/usr/local/bin"]
MIN_PYTHON = (3, 11)


def python_version_of(path):
    try:
        out = subprocess.run([path, "-c", "import sys;print(sys.version_info[0],sys.version_info[1])"],
                             capture_output=True, text=True, timeout=20).stdout.split()
        return int(out[0]), int(out[1])
    except (OSError, ValueError, IndexError, subprocess.SubprocessError):
        return (0, 0)


def abort_no_modern_python(message):
    """launchd starts system Python 3.9; with no 3.11+ on disk the run cannot start, so say so
    in the dated log, a P1 bead and a notification instead of dying in an import."""
    line = f"[ABORT] trunk-health {datetime.now().strftime('%Y-%m-%d %H:%M')}: {message}\n"
    with contextlib.suppress(OSError):
        LOG_DIR.mkdir(parents=True, exist_ok=True)
        with open(LOG_DIR / f"{datetime.now().strftime('%Y-%m-%d')}.log", "a") as f:
            f.write(line)
    sys.stderr.write(line)
    bd = shutil.which("bd", path="/opt/homebrew/bin") or "/opt/homebrew/bin/bd"
    with contextlib.suppress(OSError, subprocess.SubprocessError):
        subprocess.run([bd, "create", "trunk-health nightly cannot start: no Python 3.11+", "-t", "bug", "-p", "1",
                        "-l", "trunk-health,open", "-d", message], timeout=60)
    with contextlib.suppress(OSError, subprocess.SubprocessError):
        subprocess.run(["osascript", "-e", 'display notification "No Python 3.11+, nightly did not run" with title "MANIFOLD trunk-health"'], timeout=20)
    raise SystemExit(2)


def ensure_modern_python(version=None, tool_dirs=None, argv=None, execv=os.execv, abort=abort_no_modern_python):
    version = tuple(version if version is not None else sys.version_info[:2])[:2]
    if version >= MIN_PYTHON:
        return
    for d in (tool_dirs if tool_dirs is not None else TOOL_DIRS):
        cand = os.path.join(d, "python3")
        if os.path.isfile(cand) and os.access(cand, os.X_OK) and python_version_of(cand) >= MIN_PYTHON:
            execv(cand, [cand] + list(argv if argv is not None else sys.argv))
            return
    abort(f"running Python {version[0]}.{version[1]}; need 3.11+ (tomllib) and none found in {tool_dirs or TOOL_DIRS}")


ensure_modern_python()

import gate_passes  # noqa: E402
import gpu_queue  # noqa: E402
from storage_budget import apply_cache_cleanup, plan_cache_cleanup  # noqa: E402

BD = shutil.which("bd") or "/opt/homebrew/bin/bd"
# Cargo never deletes superseded artifacts, so main's target only grows (226G
# on 2026-09-30). Slots are capped at acquire; main is capped here, right
# before the gates rebuild it anyway.
MAIN_TARGET_CAP_GB = 100

# launchd hands a job the bare system PATH, so `cargo` and everything cargo
# shells out to are invisible to a scheduled run: every gate died with
# FileNotFoundError and filed a P1 bead blaming the gate, not the PATH. Every
# child gets this env so nested tools resolve the same toolchain.


def gate_env():
    env = os.environ.copy()
    dirs = [d for d in TOOL_DIRS if os.path.isdir(d)]
    env["PATH"] = os.pathsep.join(dirs + [env.get("PATH", "")])
    return env


def missing_tools():
    """Gate binaries this run cannot see. A loud stop beats four false reds."""
    path = gate_env()["PATH"]
    return [t for t in ("cargo", "git", "python3") if shutil.which(t, path=path) is None]


def cap_main_target(dry_run):
    """Clear main's Cargo caches when they pass the cap. The delete is
    storage_budget's exact file manifest: regular files only, no directory
    removal, no links followed, refused while any process holds the target."""
    # The cap counts only what the manifest may delete. Measured against the
    # whole folder, files it must keep could hold it over the cap forever and
    # wipe the caches every night without ever getting under.
    plan = plan_cache_cleanup(MAIN_CHECKOUT / "target")
    size = plan.bytes_total
    if plan.refusals:
        return f"[target-cap] skipped: " + "; ".join(plan.refusals[:5]) + "\n"
    if size <= MAIN_TARGET_CAP_GB * 2**30:
        return f"[target-cap] {size / 2**30:.1f}G deletable, under the {MAIN_TARGET_CAP_GB}G cap\n"
    if dry_run:
        return f"[target-cap] {size / 2**30:.1f}G deletable, over the cap; would clear caches\n"
    removed, files, failures = apply_cache_cleanup(plan, dry_run=False)
    if failures:
        return (f"[target-cap] {size / 2**30:.1f}G over the cap; removed {files} files "
                f"({removed / 2**30:.1f}G); refused: " + "; ".join(failures[:5]) + "\n")
    return (f"[target-cap] removed {files} cache files ({removed / 2**30:.1f}G) "
            f"from {size / 2**30:.1f}G\n")


def run_cmd(cmd, cwd, timeout):
    """Run subprocess, return (exit, stdout, stderr, duration)."""
    start = time.time()
    # Cargo and the feature matrix can launch nested builds.  Their process
    # creation is admitted under the same guard used by reserve(), so a
    # campaign cannot appear between a reservation check and a build launch.
    nested_build = (Path(cmd[0]).name == "cargo" or
                    any(Path(arg).name == "feature_matrix.py" for arg in cmd))
    if nested_build:
        r = gpu_queue.run_admitted(
            cmd,
            cwd=str(cwd),
            capture_output=True,
            text=True,
            timeout=timeout,
            env=gate_env(),
        )
        if r is None:
            raise ReservationDeferred
    else:
        r = subprocess.run(cmd, cwd=str(cwd), capture_output=True, text=True, timeout=timeout,
                           env=gate_env())
    duration = time.time() - start
    return r.returncode, r.stdout, r.stderr, duration


class ReservationDeferred(RuntimeError):
    """A nested build was prevented from launching by a campaign reservation."""


def reservation_message(info):
    """Human-readable persisted admission decision for a deferred nightly run."""
    end = float(info["end_epoch"])
    until = datetime.fromtimestamp(end).isoformat(timespec="seconds")
    return (f"[trunk-health] deferred: nightly GPU reservation held by "
            f"{info['owner']} for {info['reason']} until {until} ({end:.3f})")


def defer_if_reserved(log_path, log_lines, dry_run):
    info = gpu_queue.reservation()
    if not info:
        return False
    line = reservation_message(info)
    print(line)
    log_lines.append(line + "\n")
    if not dry_run:
        log_path.write_text("".join(log_lines))
    return True


def file_flaky_beads(sha):
    """One bead per flaky test: a test that failed then passed on one content key.

    Reads the gate-pass store (landing and nightly records alike). An open bead
    for the same test gets a note instead of a duplicate.
    """
    try:
        flaky = gate_passes.flaky_records(MAIN_CHECKOUT)
        result = subprocess.run([BD, "list", "--status", "open", "--json", "--flat"],
                                capture_output=True, text=True, timeout=30)
        beads = json.loads(result.stdout) if result.returncode == 0 else []
    except Exception as e:
        print(f"[trunk-health] flaky scan skipped: {e}")
        return
    by_test = {}
    for test, leg, key in flaky:
        by_test.setdefault(test, []).append((leg, key))
    for test, hits in sorted(by_test.items()):
        title = f"flaky test: {test}"
        detail = "; ".join(f"leg {leg} key {key[:16]}" for leg, key in hits)
        note = f"trunk-health flaky @{sha} ({datetime.now().strftime('%Y-%m-%d')}): failed then passed on identical content: {detail}"
        existing = next((b for b in beads if title in b.get("title", "")), None)
        try:
            if existing:
                if note in (existing.get("notes") or ""):
                    continue
                cmd = [BD, "update", existing["id"], "--append-notes", note]
            else:
                cmd = [BD, "create", title, "-t", "bug", "-p", "2", "-l", "flaky,trunk-health,open",
                       "-d", note + ". Fix by determinism or seed control, or convert a timing assertion to a work assertion (BUG-hkbdp.6.14 flake class)."]
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
            print(f"[trunk-health] flaky {'noted' if existing else 'filed'}: {test}"
                  if r.returncode == 0 else f"[trunk-health] flaky bead failed (exit {r.returncode}): {test}")
        except Exception as e:
            print(f"[trunk-health] flaky bead failed: {e}")


NIGHTLY_NEXTEST = ["cargo", "nextest", "run", "--workspace", "--no-fail-fast"]
BRIDGE_PROBE = ["python3", "scripts/bridge_probe_gate.py"]
BRIDGE_MARKER = f"\n=== {' '.join(BRIDGE_PROBE)} ===\n".strip()
BRIDGE_PERIOD_S = 7 * 86400
LAST_GREEN = LOG_DIR / "last_green.json"
INFRA_TITLE = "nightly infra: tool crash, timeout or admission refusal"
LEG_FOLD = 10
NEXTEST_LINE = re.compile(r"^\s*(PASS|FAIL|SIGSEGV|SIGABRT|SIGKILL|TIMEOUT)\s+\[[^\]]*\]\s+(?:\([^)]*\)\s+)?(\S+)\s+(\S+)\s*$")
RUNNING_LINE = re.compile(r"^\s*Running .*\(.*deps/([A-Za-z0-9_]+)-[0-9a-f]+\)\s*$")
CARGO_TEST_LINE = re.compile(r"^test (\S+) \.\.\. (ok|FAILED)\s*$")


def nightly_legs(bridge_due):
    """(info, cpu, gpu) legs. Info legs log only: they never file or turn the night red.
    The landing gate already runs the ignored-test ratchet and cargo deny."""
    info = [
        # Stale open beads get one forced verb a night.
        ["python3", "scripts/stale_beads.py"],
        # The landing loop measured over the week.
        ["python3", "scripts/landing_metrics.py", "--days", "7"],
    ]
    cpu = [
        ["cargo", "clippy", "--workspace", "--tests", "--", "-D", "warnings"],
        # Compile the default-feature suite before admission; its execution
        # stays with the GPU legs because tests may open a device transitively.
        gpu_queue.cargo_build_command(NIGHTLY_NEXTEST, "nextest-run"),
        ["python3", "scripts/feature_matrix.py"],
    ]
    gpu = [
        NIGHTLY_NEXTEST,
        # Full renderer coverage; learned timings are informational only.
        ["python3", "scripts/gpu_proofs_gate.py", "--all", "--learn-times",
         "--record-times", "/tmp/gpu_test_times.nightly.json"],
        # RT temporal stability, static then camera-orbit motion (BUG-sz0u motion leg).
        ["python3", "scripts/rt_noise_gate.py", "--require-fixture"],
        ["python3", "scripts/rt_noise_gate.py", "--motion",
         "--project", "tests/fixtures/rt/RtMotionHelmet.manifold",
         "--require-fixture"],
    ]
    if bridge_due:
        # Presentation-tear class (BUG-xaw4 tear probe), weekly.
        gpu.append(BRIDGE_PROBE)
    return info, cpu, gpu


def bridge_probe_due(log_dir, now=None):
    """Due when no nightly log from the last seven days shows the probe ran."""
    now = time.time() if now is None else now
    for log in Path(log_dir).glob("*.log"):
        with contextlib.suppress(OSError):
            if now - log.stat().st_mtime < BRIDGE_PERIOD_S and BRIDGE_MARKER in log.read_text(errors="replace"):
                return False
    return True


def parse_test_results(output):
    """([(crate, test)] failed in order, {(crate, test)} passed) from nextest or cargo test output.
    For cargo test the crate is the test binary's name, hyphenated."""
    failed, passed, binary = [], set(), "unknown"
    for line in output.splitlines():
        if m := NEXTEST_LINE.match(line):
            key = (m.group(2).split("::")[0], m.group(3))
            (passed.add(key) if m.group(1) == "PASS" else failed.append(key))
        elif m := RUNNING_LINE.match(line):
            binary = m.group(1).replace("_", "-")
        elif m := CARGO_TEST_LINE.match(line):
            key = (binary, m.group(1))
            (passed.add(key) if m.group(2) == "ok" else failed.append(key))
    return list(dict.fromkeys(failed)), passed


def infra_result(cmd_str, reason):
    return {"kind": "infra", "cmd": cmd_str, "detail": reason, "failed": [], "passed": set()}


def classify_leg(cmd_str, exit_, out, err):
    """Route one finished leg: green, failing tests, a red leg, or infra (the tool itself broke)."""
    failed, passed = parse_test_results(out + err)
    tail = "\n".join((out + err).rstrip().splitlines()[-10:])[:800]
    base = {"cmd": cmd_str, "failed": failed, "passed": passed, "detail": tail}
    if exit_ == 0:
        return dict(base, kind="green")
    if exit_ < 0 or (exit_ == 2 and cmd_str.startswith("python3") and not failed):
        # Signal death, or a gate's exit 2: measurement not made or admission refused.
        return dict(base, kind="infra", detail=f"exit {exit_}: {tail}")
    return dict(base, kind="tests" if 0 < len(failed) <= LEG_FOLD else "leg")


def bd_open_beads():
    r = subprocess.run([BD, "list", "--status", "open", "--json", "--flat"],
                       capture_output=True, text=True, timeout=30)
    return json.loads(r.stdout) if r.returncode == 0 and r.stdout.strip() else []


def suspect_commits(crate, since, sha):
    if not since:
        return "  (no green night recorded for this crate yet)"
    path = MAIN_CHECKOUT / "crates" / crate
    cmd = ["git", "log", "--oneline", "-n", "15", f"{since}..{sha}"] + (["--", str(path)] if path.is_dir() else [])
    try:
        r = subprocess.run(cmd, cwd=str(MAIN_CHECKOUT), capture_output=True, text=True, timeout=60)
        lines = r.stdout.strip().splitlines() if r.returncode == 0 else []
    except (OSError, subprocess.SubprocessError) as e:
        return f"  (git log failed: {e})"
    return "\n".join(f"  {l}" for l in lines) or "  (none touching this crate)"


def file_or_note(beads, title, desc, labels):
    """One open bead per title: create it, or append tonight's note to the open one."""
    existing = next((b for b in beads if b.get("title") == title), None)
    cmd = ([BD, "update", existing["id"], "--append-notes", desc] if existing else
           [BD, "create", title, "-t", "bug", "-p", "1", "-l", labels, "-d", desc])
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
        print(f"[trunk-health] {'noted' if existing else 'filed'}: {title}" if r.returncode == 0
              else f"[trunk-health] bead failed (exit {r.returncode}): {title}")
    except Exception as e:
        print(f"[trunk-health] bead failed: {title}: {e}")


def route_nightly(sha, results, green_legs, state_path):
    """File, note and close nightly beads.

    A failing test gets its own bead with suspect commits since its crate's last green
    sha; more than LEG_FOLD in one leg fold into one leg bead; a crash, timeout or
    refusal goes to the single infra bead. A test bead closes once that test passes."""
    date = datetime.now().strftime("%Y-%m-%d")
    try:
        last_green = json.loads(Path(state_path).read_text())
    except (OSError, ValueError):
        last_green = {}
    try:
        beads = bd_open_beads()
    except Exception as e:
        print(f"[trunk-health] bd list failed: {e}")
        beads = []
    infra = [r for r in results if r["kind"] == "infra"]
    if infra:
        body = "\n".join(f"- {r['cmd']}: {r['detail']}" for r in infra)
        file_or_note(beads, INFRA_TITLE, f"trunk-health {date} main @{sha}: the tool itself broke; main's health is unknown for:\n{body}",
                     "nightly,trunk-health,infra,open")
    for r in results:
        if r["kind"] == "tests":
            for crate, test in r["failed"]:
                since = last_green.get(crate)
                desc = (f"Summary\n  test: {test}\n  crate: {crate}\n  leg: {r['cmd']}\n  red on main @{sha} ({date})\n"
                        f"  last green for crate: {since or 'none'}\n  suspect commits since then:\n"
                        f"{suspect_commits(crate, since, sha)}\n\ntail:\n{r['detail']}")
                file_or_note(beads, f"nightly red: {crate} {test}", desc, f"nightly,trunk-health,{crate},open")
        elif r["kind"] == "leg":
            listed = "\n".join(f"  {c} {t}" for c, t in r["failed"]) or "  (no test failures parsed)"
            desc = (f"Summary\n  leg: {r['cmd']}\n  red on main @{sha} ({date})\n  failing tests ({len(r['failed'])}):\n"
                    f"{listed}\n\ntail:\n{r['detail']}")
            file_or_note(beads, f"nightly red leg: {r['cmd'][:60]}", desc, "nightly,trunk-health,open")
    passed = set().union(*(r["passed"] for r in results)) if results else set()
    failed_crates = {c for r in results for c, _ in r["failed"]}
    for crate in {c for c, _ in passed} - failed_crates:
        last_green[crate] = sha
    with contextlib.suppress(OSError):
        Path(state_path).write_text(json.dumps(last_green, indent=1, sort_keys=True))
    closing = {f"nightly red: {c} {t}" for c, t in passed}
    closing |= {f"nightly red leg: {cmd[:60]}" for cmd in green_legs}
    for bead in beads:
        title, desc = bead.get("title", ""), bead.get("description", "")
        # Legacy one-per-leg beads close on their command, as before this routing.
        legacy = "trunk-health:" in desc and any(cmd in desc for cmd in green_legs)
        if title in closing or legacy:
            try:
                r = subprocess.run([BD, "close", bead["id"]], capture_output=True, text=True, timeout=30)
                print(f"[trunk-health] closed {bead['id']} (green again)" if r.returncode == 0
                      else f"[trunk-health] failed to close {bead['id']} (exit {r.returncode})")
            except Exception as e:
                print(f"[trunk-health] exception closing {bead.get('id')}: {e}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dry-run", action="store_true",
                        help="print gate commands and simulate a red bead without running cargo or filing")
    args = parser.parse_args()

    if not args.dry_run:
        LOG_DIR.mkdir(parents=True, exist_ok=True)
    log_path = LOG_DIR / f"{datetime.now().strftime('%Y-%m-%d')}.log"
    log_lines = []

    if not args.dry_run and (missing := missing_tools()):
        print(f"[ABORT] not on PATH: {', '.join(missing)} — no gate can run, nothing filed")
        log_path.write_text(f"[ABORT] not on PATH: {', '.join(missing)}\n")
        return 2

    # A campaign may reserve the shared machine without unloading launchd.
    # Defer the complete nightly run before fetching, cleanup, or compiling;
    # the expiring record is the retry signal for the next scheduled run.
    if defer_if_reserved(log_path, log_lines, args.dry_run):
        return 0

    # Fetch origin
    if not args.dry_run:
        print(f"[trunk-health] fetching origin...")
        try:
            run_cmd(["git", "fetch", "origin"], cwd=MAIN_CHECKOUT, timeout=300)
        except subprocess.TimeoutExpired:
            print("[FAIL] git fetch timed out")
            return 2
        except Exception as e:
            print(f"[FAIL] git fetch failed: {e}")
            return 2

    sha = run_cmd(["git", "rev-parse", "--short=12", "origin/main"],
                  cwd=MAIN_CHECKOUT, timeout=300)[1].strip()
    print(f"[trunk-health] origin/main @ {sha}")
    log_lines.append(f"trunk-health for origin/main @ {sha} ({datetime.now().strftime('%Y-%m-%d')})\n")

    # Housekeeping never costs the night's gates.
    try:
        cap_line = cap_main_target(args.dry_run)
    except Exception as e:
        cap_line = f"[target-cap] failed, skipped: {e}\n"
    print(cap_line, end="")
    log_lines.append(cap_line)

    info_gates, cpu_gates, gpu_gates = nightly_legs(bridge_probe_due(LOG_DIR))

    green_gates = []
    results = []

    for cmd in info_gates:
        if defer_if_reserved(log_path, log_lines, args.dry_run):
            return 0
        cmd_str = " ".join(cmd)
        print(f"[trunk-health] info {cmd_str}...")
        log_lines.append(f"\n=== {cmd_str} (info only) ===\n")
        if args.dry_run:
            print(f"[dry-run] would run (info only, never files): {cmd_str}")
            continue
        try:
            exit_, out, err, _ = run_cmd(cmd, cwd=MAIN_CHECKOUT, timeout=600)
            log_lines.append(out + err + f"\n[INFO exit {exit_}]\n")
        except Exception as e:
            log_lines.append(f"[INFO skipped: {e}]\n")

    def run_gate(cmd):
        cmd_str = " ".join(cmd)
        print(f"[trunk-health] running {cmd_str}...")
        header = f"\n=== {cmd_str} ===\n"
        log_lines.append(header)

        if args.dry_run:
            # Simulate one failing test on the nextest leg to show routing.
            if cmd is NIGHTLY_NEXTEST:
                print(f"[dry-run] would run: {cmd_str}")
                fake = "        FAIL [   0.1s] manifold-core simulated::test\n"
                results.append(classify_leg(cmd_str, 100, fake, ""))
                print(f"[dry-run] [FAIL] {cmd_str} (simulated red)")
                return
            print(f"[dry-run] would run: {cmd_str}")
            print(f"[dry-run] [PASS] {cmd_str}")
            log_lines.append(f"[PASS] {cmd_str}\n")
            green_gates.append(cmd_str)
            return

        try:
            exit_, out, err, duration = run_cmd(cmd, cwd=MAIN_CHECKOUT, timeout=5400)
            deferred = exit_ == 0 and "[DEFER]" in out
            status = "DEFER" if deferred else ("PASS" if exit_ == 0 else "FAIL")
            print(f"[{status}] {cmd_str} ({duration:.0f}s)")
            log_lines.append(out + err + f"\n[{status}] ({duration:.0f}s)\n")

            # feature_matrix exits cleanly after an atomic admission refusal;
            # keep that retry signal from looking like a green gate that could
            # close an existing red bead.
            if deferred:
                if not args.dry_run:
                    log_path.write_text("".join(log_lines))
                return 3

            result = classify_leg(cmd_str, exit_, out, err)
            results.append(result)
            if result["kind"] == "green":
                green_gates.append(cmd_str)
        except subprocess.TimeoutExpired:
            print(f"[FAIL] {cmd_str} (timed out)")
            log_lines.append(f"[FAIL] (timed out)\n")
            results.append(infra_result(cmd_str, "timed out"))
        except ReservationDeferred:
            line = "[trunk-health] deferred: nightly GPU reservation became active"
            print(line)
            log_lines.append(line + "\n")
            if not args.dry_run:
                log_path.write_text("".join(log_lines))
            return 3
        except FileNotFoundError as e:
            # The gate never ran, so main's health is unknown. Filing a red
            # bead here blames the gate for the environment and buries the
            # real signal under P1 noise.
            print(f"[ABORT] cannot run {cmd_str}: {e}")
            log_lines.append(f"[ABORT] cannot run: {e}\n")
            log_path.write_text("".join(log_lines))
            return 2
        except Exception as e:
            print(f"[FAIL] {cmd_str} ({e})")
            log_lines.append(f"[FAIL] ({e})\n")
            results.append(infra_result(cmd_str, f"crashed: {str(e)[:800]}"))

    for cmd in cpu_gates:
        if defer_if_reserved(log_path, log_lines, args.dry_run):
            return 0
        result = run_gate(cmd)
        if result == 2:
            return 2
        if result == 3:
            return 0
    # Each GPU leg is independent. Releasing between legs lets an announced
    # landing take the next admission while a proof already in flight finishes.
    for cmd in gpu_gates:
        if defer_if_reserved(log_path, log_lines, args.dry_run):
            return 0
        gpu_hold = (contextlib.nullcontext() if args.dry_run else
                    gpu_queue.hold("trunk_health gpu legs", priority="nightly"))
        with gpu_hold:
            result = run_gate(cmd)
            if result == 2:
                return 2
            if result == 3:
                return 0

    if args.dry_run:
        for r in results:
            names = [f"nightly red: {c} {t}" for c, t in r["failed"]] if r["kind"] == "tests" else [r["kind"]]
            print(f"[dry-run] would route {r['cmd']}: {', '.join(names)}")
        print(f"[dry-run] would close beads for green legs: {', '.join(green_gates)}")
        return 0

    # Hook-telemetry census — informational only, never files a bead or fails.
    print(f"[trunk-health] running hook census...")
    header = "\n=== hook census (7-day window) ===\n"
    log_lines.append(header)
    try:
        census_cmd = ["python3", "scripts/hook_census.py", "--days", "7"]
        _, census_out, _, _ = run_cmd(census_cmd, cwd=MAIN_CHECKOUT, timeout=120)
        print(census_out)
        log_lines.append(census_out)
    except Exception as e:
        msg = f"[trunk-health] hook census skipped: {e}\n"
        print(msg)
        log_lines.append(msg)

    # Write log
    with open(log_path, "a") as f:
        f.writelines(log_lines)
    print(f"[trunk-health] log written to {log_path}")

    if not BD:
        print(f"[trunk-health] [FAIL] cannot file beads: bd binary not found")
        return 2
    route_nightly(sha, results, green_gates, LAST_GREEN)
    file_flaky_beads(sha)

    red = [r["cmd"][:50] for r in results if r["kind"] != "green"]
    if red:
        print(f"[trunk-health] RED: {', '.join(red)}")
        return 1
    print(f"[trunk-health] green: all gates passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
