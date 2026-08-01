#!/usr/bin/env python3
"""bridge-probe regression gate (BUG-uggg) — the presentation-tear class stays detectable.

bridge-probe (crates/manifold-app/src/bridge_probe.rs, perf-soak feature) is the
red/green harness for BUG-xaw4 (presentation-transport audit): a writer thread
and a reader thread hammer a SharedTextureBridge on two MTLDevices, counting
reads that sampled a surface mid-write.

Two legs, both load-bearing:
  --policy legacy MUST tear (exit 1, "RACE PRESENT") — the pre-fix contract.
  If legacy ever goes clean the probe has gone blind (timing shift, scheduler
  change) and the fenced leg's CLEAN is vacuous.
  --policy fenced MUST be clean (exit 0, "CLEAN") — the shipped read-fence
  contract (docs/VSYNC_AND_FRAME_PACING.md "Read fence").

Debug binary, not release: the probe's race window is timing-based and was
validated (and Peter-confirmed) against the debug build.

Exit: 0 both legs as expected; 1 contract breach (regression = bead); 2 the
measurement could not be made (build failure, missing verdict markers).

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


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--frames", type=int, default=600,
                    help="per-leg frame count (default 600, the probe default)")
    args = ap.parse_args()

    log("building manifold (debug, perf-soak)...")
    code, out, dur = run(["cargo", "build", "-p", "manifold-app", "--features",
                          "perf-soak", "--bin", "manifold"], timeout=3600)
    if code != 0 or not BINARY.exists():
        print(out[-3000:])
        log("EXIT 2: build failed")
        return 2
    log(f"built ({dur:.0f}s)")

    failures = []

    code, out, dur = run([str(BINARY), "bridge-probe", "--policy", "legacy",
                          "--frames", str(args.frames)], timeout=900)
    tail = out.strip().splitlines()[-3:]
    log(f"legacy leg: exit={code} ({dur:.0f}s) — {' / '.join(tail)}")
    if "VERDICT:" not in out:
        log("EXIT 2: legacy leg produced no verdict — measurement not made")
        return 2
    if code != 1 or "RACE PRESENT" not in out:
        failures.append("legacy leg did NOT tear — probe has gone blind, "
                        "the fenced CLEAN below proves nothing")

    code, out, dur = run([str(BINARY), "bridge-probe", "--policy", "fenced",
                          "--frames", str(args.frames)], timeout=900)
    tail = out.strip().splitlines()[-3:]
    log(f"fenced leg: exit={code} ({dur:.0f}s) — {' / '.join(tail)}")
    if "VERDICT:" not in out:
        log("EXIT 2: fenced leg produced no verdict — measurement not made")
        return 2
    if code != 0 or "CLEAN" not in out:
        failures.append("fenced leg TORE — the BUG-xaw4 read-fence contract "
                        "is broken (presentation tear class is back)")

    if failures:
        for f in failures:
            log(f"FAIL: {f}")
        return 1
    log("green: legacy tears, fenced clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
