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
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path
import shutil

import gpu_queue
from storage_budget import apply_cache_cleanup, plan_cache_cleanup

MAIN_CHECKOUT = Path("/Users/peterkiemann/MANIFOLD - Rust")
LOG_DIR = MAIN_CHECKOUT / ".claude/orchestration/trunk-health"
BD = shutil.which("bd") or "/opt/homebrew/bin/bd"
# Cargo never deletes superseded artifacts, so main's target only grows (226G
# on 2026-09-30). Slots are capped at acquire; main is capped here, right
# before the gates rebuild it anyway.
MAIN_TARGET_CAP_GB = 100

# launchd hands a job the bare system PATH, so `cargo` and everything cargo
# shells out to are invisible to a scheduled run: every gate died with
# FileNotFoundError and filed a P1 bead blaming the gate, not the PATH. Every
# child gets this env so nested tools resolve the same toolchain.
TOOL_DIRS = [os.path.expanduser("~/.cargo/bin"), "/opt/homebrew/bin", "/usr/local/bin"]


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

    nightly_nextest = ["cargo", "nextest", "run", "--workspace", "--no-fail-fast"]
    cpu_gates = [
        # Ignored-test ratchet: any #[ignore] beyond the baseline is a red
        # gate made invisible (SCENE_LOOP shipped broken behind one).
        # Cheap, runs first so it files before the long legs.
        ["python3", ".claude/hooks/ignored-test-guard.py", "--scan"],
        # Stale open beads get one forced verb a night (fix, demote, close)
        # instead of a nag at every session start.
        ["python3", "scripts/stale_beads.py"],
        # The landing loop measured: runs per landing, red share, GPU reuse
        # and queue wait over the week. Red when reds are found by rerunning
        # the gate instead of its printed rerun commands.
        ["python3", "scripts/landing_metrics.py", "--days", "7"],
        ["cargo", "clippy", "--workspace", "--tests", "--", "-D", "warnings"],
        # Compile the default-feature suite before admission. Its execution
        # stays with the nightly GPU legs because selected tests may open a
        # device transitively and must yield to an announced landing.
        [*nightly_nextest, "--no-run"],
        ["cargo", "deny", "check", "bans"],
        ["python3", "scripts/feature_matrix.py"],
    ]
    gpu_gates = [
        nightly_nextest,
        # Full renderer coverage, including the asset-conformance sweep, belongs
        # here (landing runs only the scoped set; see scripts/gpu_scope.py).
        # Export successful keyed measurements for review. Learned timings are
        # informational; only committed timings set landing watchdog allowances.
        ["python3", "scripts/gpu_proofs_gate.py", "--all", "--learn-times",
         "--record-times", "/tmp/gpu_test_times.nightly.json"],
        # RT temporal stability. Nightly and not at landing: it costs an app
        # build plus a 300-frame render, three times over. Skips green (loudly)
        # while its ceilings are unvalidated, so it files no beads until the
        # numbers mean something.
        ["python3", "scripts/rt_noise_gate.py", "--require-fixture"],
        # RT MOTION leg (BUG-sz0u): camera-orbit ramp on the helmet fixture —
        # the coverage that catches motion-only regressions (D-64 boil,
        # sv-gate re-trip) the static legs can't see. Red-validated against
        # boil-era gate behavior before this wiring (sv_hold median 4x over
        # the healthy ceiling with D-64/tr5o reverted). The fixture is
        # RtMotionHelmet.manifold, NOT RtNoiseTesting: the noise fixture
        # shows no red/green separation for this class (measured 2026-08-01).
        ["python3", "scripts/rt_noise_gate.py", "--motion",
         "--project", "tests/fixtures/rt/RtMotionHelmet.manifold",
         "--require-fixture"],
        # Presentation-tear class (BUG-xaw4): legacy policy must keep tearing
        # (probe not blind) AND fenced policy must stay clean (the shipped
        # read-fence contract). Independent legs yield to pending landings.
        ["python3", "scripts/bridge_probe_gate.py"],
    ]

    green_gates = []
    red_gates = []

    def run_gate(cmd):
        cmd_str = " ".join(cmd)
        print(f"[trunk-health] running {cmd_str}...")
        header = f"\n=== {cmd_str} ===\n"
        log_lines.append(header)

        if args.dry_run:
            # Simulate ONE fake red gate for dedupe logic demo
            if cmd_str.startswith("cargo deny"):
                print(f"[dry-run] would run: {cmd_str}")
                print(f"[dry-run] [FAIL] cargo deny check bans (simulated red)")
                red_gates.append((cmd_str, "[FAIL] cargo deny check bans (simulated red)", "simulated tail\nline 2\nline 3"))
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

            if exit_ != 0:
                tail = (out + err).rstrip().splitlines()[-10:]
                tail_str = "\n".join(tail)[:800]
                red_gates.append((cmd_str, f"[FAIL] {cmd_str}", tail_str))
            else:
                green_gates.append(cmd_str)
        except subprocess.TimeoutExpired:
            print(f"[FAIL] {cmd_str} (timed out)")
            log_lines.append(f"[FAIL] (timed out)\n")
            red_gates.append((cmd_str, f"[FAIL] {cmd_str} (timed out)", "timeout"))
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
            red_gates.append((cmd_str, f"[FAIL] {cmd_str}", str(e)[:800]))

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
        # Simulate bead dedupe logic for the fake red gate
        print("[dry-run] checking for existing trunk-health beads...")
        if not BD:
            print("[dry-run] [bd binary not found, would exit 2]")
            return 2

        try:
            result = subprocess.run([BD, "list", "--status", "open", "--json", "--flat"],
                                   capture_output=True, text=True, timeout=30)
            if result.returncode == 0:
                beads = result.stdout.strip().splitlines()
                for line in beads:
                    if "trunk-health:" in line and "cargo deny check bans" in line:
                        print(f"[dry-run] existing bead found, would skip filing: {line[:100]}")
                        return 0
        except Exception as e:
            print(f"[dry-run] bd list failed: {e}")

        print("[dry-run] would file bead:")
        cmd_str = "cargo deny check bans"
        desc = f"trunk-health: {cmd_str} red on main @{sha} ({datetime.now().strftime('%Y-%m-%d')}); tail: simulated tail\nline 2\nline 3"
        print(f"  bd create \"trunk-health red: {cmd_str[:60]}\" -t bug -p 1 -l trunk-health,open -d '{desc}'")

        # Simulate bead auto-close for green gates
        print("[dry-run] would close beads for green gates:")
        for green_cmd in green_gates:
            print(f"[dry-run] would close beads matching trunk-health: {green_cmd}")
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

    # File beads for red gates
    for cmd_str, status_line, tail in red_gates:
        if not BD:
            print(f"[trunk-health] [FAIL] cannot file bead: bd binary not found at {BD}")
            return 2

        # Dedupe check
        already_filed = False
        try:
            result = subprocess.run([BD, "list", "--status", "open", "--json", "--flat"],
                                   capture_output=True, text=True, timeout=30)
            if result.returncode == 0:
                for line in result.stdout.strip().splitlines():
                    if "trunk-health:" in line and cmd_str in line:
                        print(f"[trunk-health] existing bead found, skipping: {line[:100]}")
                        already_filed = True
                        break
        except Exception as e:
            print(f"[trunk-health] dedupe check failed: {e}")
        if already_filed:
            continue

        # File new bead
        title = f"trunk-health red: {cmd_str[:60]}"
        desc = f"trunk-health: {cmd_str} red on main @{sha} ({datetime.now().strftime('%Y-%m-%d')}); tail: {tail}"
        try:
            r = subprocess.run([BD, "create", title, "-t", "bug", "-p", "1", "-l", "trunk-health,open", "-d", desc],
                              capture_output=True, text=True, timeout=30)
            if r.returncode != 0:
                print(f"[trunk-health] failed to file bead (exit {r.returncode}): {(r.stderr or '').strip()[:200]}")
                return 2
            print(f"[trunk-health] filed bead: {title}")
        except Exception as e:
            print(f"[trunk-health] failed to file bead: {e}")
            return 2

    # Auto-close beads for green gates — a gate that recovered closes its own
    # bead even when another gate is still red.
    try:
        result = subprocess.run([BD, "list", "--status", "open", "--json", "--flat"],
                               capture_output=True, text=True, timeout=30)
        beads = json.loads(result.stdout) if result.returncode == 0 else []
    except Exception as e:
        print(f"[trunk-health] bd list failed for bead close: {e}")
        beads = []
    for green_cmd in green_gates:
        for bead in beads:
            description = bead.get("description", "")
            if "trunk-health:" in description and green_cmd in description:
                bead_id = bead.get("id")
                try:
                    r = subprocess.run([BD, "close", bead_id],
                                    capture_output=True, text=True, timeout=30)
                    if r.returncode == 0:
                        print(f"[trunk-health] closed {bead_id} (gate green again)")
                    else:
                        print(f"[trunk-health] failed to close {bead_id} (exit {r.returncode})")
                except Exception as e:
                    print(f"[trunk-health] exception closing {bead_id}: {e}")

    if red_gates:
        red_names = ", ".join(cmd_str[:50] for cmd_str, _, _ in red_gates)
        print(f"[trunk-health] RED: {red_names}")
        return 1
    print(f"[trunk-health] green: all gates passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
