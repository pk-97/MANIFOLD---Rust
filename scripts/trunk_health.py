#!/usr/bin/env python3
"""Nightly trunk-health sweep — the workspace-wide checks that used to run at every landing.

Moved out per GIT_TREE_DISCIPLINE.md section 2 (Landing protocol), 2026-07-29.
Runs in the main checkout against current main; a red gate files a P1 trunk-health bead.
Scheduled by launchd (scripts/com.manifold.trunk-health.plist).
"""

import argparse
import json
import os
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path
import shutil

MAIN_CHECKOUT = Path("/Users/peterkiemann/MANIFOLD - Rust")
LOG_DIR = MAIN_CHECKOUT / ".claude/orchestration/trunk-health"
BD = shutil.which("bd") or "/opt/homebrew/bin/bd"

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


def run_cmd(cmd, cwd, timeout):
    """Run subprocess, return (exit, stdout, stderr, duration)."""
    start = time.time()
    r = subprocess.run(cmd, cwd=str(cwd), capture_output=True, text=True, timeout=timeout,
                       env=gate_env())
    duration = time.time() - start
    return r.returncode, r.stdout, r.stderr, duration


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dry-run", action="store_true",
                        help="print gate commands and simulate a red bead without running cargo or filing")
    args = parser.parse_args()

    LOG_DIR.mkdir(parents=True, exist_ok=True)
    log_path = LOG_DIR / f"{datetime.now().strftime('%Y-%m-%d')}.log"
    log_lines = []

    if not args.dry_run and (missing := missing_tools()):
        print(f"[ABORT] not on PATH: {', '.join(missing)} — no gate can run, nothing filed")
        log_path.write_text(f"[ABORT] not on PATH: {', '.join(missing)}\n")
        return 2

    # Fetch origin
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

    gates = [
        ["cargo", "clippy", "--workspace", "--tests", "--", "-D", "warnings"],
        ["cargo", "nextest", "run", "--workspace"],
        ["cargo", "deny", "check", "bans"],
        ["python3", "scripts/feature_matrix.py"],
        # RT temporal stability. Nightly and not at landing: it costs an app
        # build plus a 300-frame render, three times over. Skips green (loudly)
        # while its ceilings are unvalidated, so it files no beads until the
        # numbers mean something.
        ["python3", "scripts/rt_noise_gate.py", "--require-fixture"],
    ]

    green_gates = []
    red_gates = []

    for cmd in gates:
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
                continue
            print(f"[dry-run] would run: {cmd_str}")
            print(f"[dry-run] [PASS] {cmd_str}")
            log_lines.append(f"[PASS] {cmd_str}\n")
            green_gates.append(cmd_str)
            continue

        try:
            exit_, out, err, duration = run_cmd(cmd, cwd=MAIN_CHECKOUT, timeout=5400)
            status = "PASS" if exit_ == 0 else "FAIL"
            print(f"[{status}] {cmd_str} ({duration:.0f}s)")
            log_lines.append(out + err + f"\n[{status}] ({duration:.0f}s)\n")

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
