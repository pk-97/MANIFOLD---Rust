#!/usr/bin/env python3
"""bridge-probe regression gate (BUG-4qob) — probabilistic liveness, strict safety.

bridge-probe (crates/manifold-app/src/bridge_probe.rs, perf-soak feature) is the
red/green harness for BUG-xaw4 (presentation-transport audit): a writer thread
and a reader thread hammer a SharedTextureBridge on two MTLDevices, counting
reads that sampled a surface mid-write.

Probabilistic-LIVENESS, strict-SAFETY semantics:
  --policy legacy MUST tear at least once across 7 runs — the pre-fix contract.
  System timing non-determinism on current hardware makes single-run detection
  unreliable (2026-08-03 investigation). 7 runs give 94% sensitivity at p=1/3.
  --policy fenced MUST be clean across all 7 runs — the shipped read-fence
  contract (docs/VSYNC_AND_FRAME_PACING.md "Read fence"). A single fenced tear
  fails the gate (strict safety — regression = bead, BUG-m0c9).

GPU-busy pre-check: Exits loud if concurrent GPU work detected (cargo test
with gpu-proofs, another bridge-probe, manifold renders). GPU gates own the
machine — never a silent pass or skip.

Debug binary, not release: the probe's race window is timing-based and was
validated (and Peter-confirmed) against the debug build.

Exit: 0 both legs as expected; 1 contract breach (regression = bead); 2 the
measurement could not be made (build failure, missing verdict markers, GPU busy).

Nightly on main via scripts/trunk_health.py — never in the default suite
(costs an app build plus GPU seconds). GPU-serial by trunk_health's gates
loop; the probe writes no capture dir, so no MANIFOLD_RT_CAPTURE_DIR-style
isolation is needed.
"""

import argparse
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BINARY = REPO / "target/debug/manifold"


def log(msg):
    print(f"[bridge-probe-gate] {msg}", flush=True)


def run(cmd, timeout):
    start = time.time()
    r = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True, timeout=timeout)
    return r.returncode, r.stdout + r.stderr, time.time() - start


def check_gpu_busy():
    """Check for concurrent GPU work — exits loud if GPU processes detected (BUG-m0c9).

    Matches on process cmdline (not just process names) so cargo test
    invocations carrying gpu-proofs / manifold_renderer, a running
    bridge-probe binary, and manifold render/capture runs are all caught.
    """
    gpu_processes = []
    try:
        # `ps aux -o pid=,command=` gives PID + full command line per process.
        out = subprocess.run(
            ["ps", "aux", "-o", "pid=,command="],
            capture_output=True, text=True, timeout=30,
        ).stdout
    except (subprocess.TimeoutExpired, OSError) as e:
        log(f"WARNING: could not enumerate processes ({e}); skipping GPU-busy check")
        return True

    for line in out.splitlines():
        line = line.strip()
        if not line:
            continue
        parts = line.split(None, 1)
        if len(parts) != 2:
            continue
        pid, cmd = parts
        if not cmd:
            continue
        # Skip this script's own process.
        if cmd.startswith("python") and "bridge_probe_gate" in cmd:
            continue

        if _is_gpu_process(cmd):
            gpu_processes.append((pid, cmd[:200]))

    if gpu_processes:
        log("EXIT 2: concurrent GPU process detected — GPU gates own the machine (BUG-m0c9)")
        for pid, cmd in gpu_processes:
            print(f"  PID {pid}: {cmd}")
        return False
    return True


def _is_gpu_process(cmd):
    """Return True if a process command line indicates GPU work."""
    if "cargo" in cmd and "test" in cmd and (
        "gpu-proofs" in cmd
        or "manifold_renderer" in cmd
        or "--features gpu" in cmd
    ):
        return True
    if "bridge-probe" in cmd and "manifold" in cmd:
        return True
    if "manifold" in cmd and any(k in cmd for k in (
        "bridge-probe", "--capture", "rt-capture", "render",
    )):
        return True
    return False


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--frames", type=int, default=600,
                    help="per-leg frame count (default 600, the probe default)")
    args = ap.parse_args()

    log("checking for concurrent GPU work...")
    if not check_gpu_busy():
        return 2

    log("building manifold (debug, perf-soak)...")
    code, out, dur = run(["cargo", "build", "-p", "manifold-app", "--features",
                          "perf-soak", "--bin", "manifold"], timeout=3600)
    if code != 0 or not BINARY.exists():
        print(out[-3000:])
        log("EXIT 2: build failed")
        return 2
    log(f"built ({dur:.0f}s)")

    failures = []

    # Legacy leg: 7 runs, require ≥1 tear (probabilistic liveness)
    legacy_tears = 0
    legacy_runs = 7
    for i in range(legacy_runs):
        code, out, dur = run([str(BINARY), "bridge-probe", "--policy", "legacy",
                              "--frames", str(args.frames)], timeout=900)
        tail = out.strip().splitlines()[-3:]
        log(f"legacy run {i+1}/{legacy_runs}: exit={code} ({dur:.0f}s) — {' / '.join(tail)}")
        if "VERDICT:" not in out:
            log("EXIT 2: legacy leg produced no verdict — measurement not made")
            return 2
        if code == 1 and "RACE PRESENT" in out:
            legacy_tears += 1
        elif code == 0 and "CLEAN" in out:
            pass  # No tear this run
        else:
            failures.append(f"legacy run {i+1} unexpected exit={code}")

    log(f"legacy leg: {legacy_tears}/{legacy_runs} runs tore")
    if legacy_tears == 0:
        failures.append("legacy leg NEVER tore across 7 runs — probe has gone blind, "
                        "fenced CLEAN below proves nothing")

    # Fenced leg: 7 runs, require 0 tears (strict safety)
    fenced_tears = 0
    fenced_runs = 7
    for i in range(fenced_runs):
        code, out, dur = run([str(BINARY), "bridge-probe", "--policy", "fenced",
                              "--frames", str(args.frames)], timeout=900)
        tail = out.strip().splitlines()[-3:]
        log(f"fenced run {i+1}/{fenced_runs}: exit={code} ({dur:.0f}s) — {' / '.join(tail)}")
        if "VERDICT:" not in out:
            log("EXIT 2: fenced leg produced no verdict — measurement not made")
            return 2
        if code == 1 and "RACE PRESENT" in out:
            fenced_tears += 1
            failures.append(f"fenced run {i+1} TORE — BUG-xaw4 read-fence contract broken")
        elif code == 0 and "CLEAN" in out:
            pass  # Clean as expected
        else:
            failures.append(f"fenced run {i+1} unexpected exit={code}")

    log(f"fenced leg: {fenced_tears}/{fenced_runs} runs tore (MUST be 0)")

    if failures:
        for f in failures:
            log(f"FAIL: {f}")
        return 1
    log(f"green: legacy tore {legacy_tears}/{legacy_runs} runs, fenced clean {fenced_runs}/{fenced_runs}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
