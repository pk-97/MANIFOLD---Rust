#!/usr/bin/env python3
"""RT A2 — per-term native-resolution frame-cost measurement.

Measures steady-state content frame time for the single shared RT dispatch at
half-res (today's behavior) vs native (MANIFOLD_RT_NATIVE_TERMS=shadow,ao,gi,
reflection). Answers the budget question: does running the whole RT dispatch at
native 4K fit the 41.6ms (24fps) ceiling?

ARCHITECTURE NOTE (why this is two configs, not four):
  All four RT terms (shadow vis, AO, GI, reflection) are written by ONE Metal
  dispatch — `tracer.dispatch_shadow_rays(...)` in render_scene.rs, with a
  single `trace_size`. Listing ANY term in MANIFOLD_RT_NATIVE_TERMS nativizes
  that whole dispatch; per-term resolution isolation requires splitting the
  dispatch and is deferred to A3. So the meaningful comparison is baseline
  (all half-res) vs all-native.

Protocol:
  For each fixture x lighting x {baseline, all-native}:
    manifold rt-capture <fixture> --width 3840 --height 2160 --frames 150 \
        --sync-gpu --set-at 10 <lighting snaps>
    with MANIFOLD_RT_NATIVE_TERMS set per config. `--sync-gpu` blocks on the
    GPU fence each frame and prints `[GPU_FRAME_MS] frame=N ms=X.XX` — the
    per-frame GPU cost (the metric a 41.6ms/24fps budget is measured against).
    Note: MANIFOLD_RENDER_TRACE's `[RENDER_TRACE]` measures CPU encode only
    (the content thread pipelines), so it is NOT the budget metric.
  Discard the first 30 frames (warmup: accel build + JIT), take the last 120.
  Report median + p95 GPU ms.

Output: scripts/rt_a2_term_cost.json — per fixture x lighting x config ->
median_ms, p95_ms, delta_ms, fits_41_6ms; plus a verdicts array.
"""

import json
import os
import re
import statistics
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

# ── config ──────────────────────────────────────────────────────────────────
REPO = Path("/Users/peterkiemann/MANIFOLD - Rust")
SLOT = Path(__file__).resolve().parent.parent  # worktree root that holds this script
BINARY = SLOT / "target/release/manifold"
OUT_JSON = SLOT / "scripts/rt_a2_term_cost.json"

WIDTH, HEIGHT = 3840, 2160
TOTAL_FRAMES = 150          # 30 warmup + 120 measured
WARMUP = 30
MEASURE = TOTAL_FRAMES - WARMUP
FRAME_BUDGET_MS = 41.6      # 24fps ceiling

# Lighting snaps resolve on the Helmet generator param set (8_rt_enabled,
# 1_emitter_intensity, 1_sun_x) — same for the recreated Apricot (helmet copy
# + apricot glb). Source: LIGHTING_CONFIGS in scripts/rt_quality_matrix.py.
LIGHTING = {
    "sun-only": ["8_rt_enabled=1.0", "1_emitter_intensity=0.0"],
    "env-only": ["8_rt_enabled=1.0", "1_emitter_intensity=3.0", "1_sun_x=-10.0"],
}

CONFIGS = {
    "baseline":    "",                         # D11 half-res for all four terms
    "all-native":  "shadow,ao,gi,reflection",  # nativize the shared dispatch
}

FIXTURES = {
    "RtMotionHelmet": "tests/fixtures/rt/RtMotionHelmet.manifold",
    "RtApricot":      "tests/fixtures/rt/RtApricot.manifold",
}

_FRAME_RE = re.compile(r"\[GPU_FRAME_MS\] frame=(\d+) ms=([\d.]+)")


def log(msg):
    print(f"[{datetime.now(timezone.utc).isoformat(timespec='seconds')}] {msg}",
          flush=True)


def parse_frame_times(stderr: str):
    """Return per-frame ms keyed by frame index (only [FRAME_TIME] lines)."""
    times = {}
    for line in stderr.splitlines():
        m = _FRAME_RE.search(line)
        if m:
            times[int(m.group(1))] = float(m.group(2))
    return times


def run_one(fixture_rel: str, lighting_snaps, native_terms: str):
    """Run rt-capture once; return (frame_times_dict, stderr_tail, ok)."""
    fixture_abs = str((SLOT / fixture_rel).resolve())
    env = os.environ.copy()
    env["MANIFOLD_RT_NATIVE_TERMS"] = native_terms
    # Unique capture dir per run so two runs never interleave their PNGs.
    env["MANIFOLD_RT_CAPTURE_DIR"] = f"/tmp/rt_a2_{os.getpid()}_{int(time.time()*1000)}"

    cmd = [
        str(BINARY), "rt-capture",
        str(fixture_abs),                       # project path MUST be first
        "--width", str(WIDTH), "--height", str(HEIGHT),
        "--sync-gpu",
        "--frames", str(TOTAL_FRAMES),
    ]
    # Apply lighting at frame 10 (after first accel build, before measurement window).
    for snap in lighting_snaps:
        cmd += ["--set-at", "10", snap]

    log(f"  cmd: MANIFOLD_RT_NATIVE_TERMS='{native_terms}' rt-capture ... {Path(fixture_rel).name}")
    try:
        r = subprocess.run(cmd, env=env, capture_output=True, text=True,
                           cwd=str(SLOT), timeout=240)
    except subprocess.TimeoutExpired:
        return {}, "TIMEOUT after 900s", False
    ft = parse_frame_times(r.stderr)
    return ft, r.stderr[-1200:], r.returncode == 0


def stats_for(frame_times):
    """Take measured window (frames >= WARMUP), return median/p95/list or None."""
    measured = [ms for f, ms in frame_times.items() if f >= WARMUP]
    if len(measured) < MEASURE * 0.9:   # allow a few dropped lines
        return None
    measured.sort()
    median = statistics.median(measured)
    # p95 by nearest-rank on the measured window
    p95 = measured[min(len(measured) - 1, int(round(0.95 * (len(measured) - 1))))]
    return median, p95, measured


def main():
    if not BINARY.exists():
        sys.exit(f"missing release binary: {BINARY}\n"
                 f"build with: cargo build --release -p manifold-app "
                 f"--features perf-soak --bin manifold")

    results = {
        "schema": 1,
        "metadata": {
            "width": WIDTH,
            "height": HEIGHT,
            "total_frames": TOTAL_FRAMES,
            "warmup_frames": WARMUP,
            "measure_frames": MEASURE,
            "frame_budget_ms": FRAME_BUDGET_MS,
            "binary": str(BINARY),
            "recorded": datetime.now(timezone.utc).date().isoformat(),
        },
        "note": (
            "All four RT terms (shadow vis, AO, GI, reflection) share ONE Metal "
            "dispatch (dispatch_shadow_rays) with a single trace_size, so any "
            "listed term nativizes the whole dispatch. Per-term resolution "
            "attribution requires splitting that dispatch — deferred to A3. "
            "Hence baseline (half-res) vs all-native, not per-term."
        ),
        "measurements": [],
        "verdicts": [],
    }

    for fx_name, fx_rel in FIXTURES.items():
        if not (SLOT / fx_rel).exists():
            log(f"SKIP fixture {fx_name}: {fx_rel} not found")
            results["measurements"].append({
                "fixture": fx_name, "lighting": "n/a", "config": "n/a",
                "error": f"fixture not found: {fx_rel}",
            })
            continue

        for light_name, snaps in LIGHTING.items():
            log(f"{fx_name} / {light_name}")

            per = {}
            for cfg_name, native_terms in CONFIGS.items():
                ft, tail, ok = run_one(fx_rel, snaps, native_terms)
                if not ok:
                    log(f"  {cfg_name}: RUN FAILED (rc!=0). tail:\n{tail[-400:]}")
                    results["measurements"].append({
                        "fixture": fx_name, "lighting": light_name,
                        "config": cfg_name, "native_terms": native_terms,
                        "error": "run failed", "stderr_tail": tail[-400:],
                    })
                    continue
                st = stats_for(ft)
                if st is None:
                    log(f"  {cfg_name}: only {sum(1 for f in ft if f >= WARMUP)} "
                        f"measured frames (need ~{MEASURE}). tail:\n{tail[-400:]}")
                    results["measurements"].append({
                        "fixture": fx_name, "lighting": light_name,
                        "config": cfg_name, "native_terms": native_terms,
                        "error": "insufficient frames",
                        "frames_seen": sum(1 for f in ft if f >= WARMUP),
                        "stderr_tail": tail[-400:],
                    })
                    continue
                median, p95, measured = st
                per[cfg_name] = median
                log(f"  {cfg_name}: median={median:.2f}ms p95={p95:.2f}ms "
                    f"(n={len(measured)})")
                results["measurements"].append({
                    "fixture": fx_name, "lighting": light_name,
                    "config": cfg_name, "native_terms": native_terms,
                    "median_ms": round(median, 2),
                    "p95_ms": round(p95, 2),
                    "n": len(measured),
                })

            if "baseline" in per and "all-native" in per:
                base = per["baseline"]
                native = per["all-native"]
                delta = native - base
                fits = native <= FRAME_BUDGET_MS
                results["measurements"][-1]["delta_vs_baseline_ms"] = round(delta, 2)
                # backfill delta on the all-native record
                for m in reversed(results["measurements"]):
                    if (m.get("fixture") == fx_name and m.get("lighting") == light_name
                            and m.get("config") == "all-native"):
                        m["delta_vs_baseline_ms"] = round(delta, 2)
                        m["fits_41_6ms"] = fits
                        break
                verdict = ("FITS" if fits else "EXCEEDS") + " budget"
                results["verdicts"].append({
                    "fixture": fx_name, "lighting": light_name,
                    "baseline_median_ms": round(base, 2),
                    "all_native_median_ms": round(native, 2),
                    "delta_ms": round(delta, 2),
                    "fits_41_6ms": fits,
                    "verdict": verdict,
                })
                log(f"  -> delta={delta:+.2f}ms all-native {verdict}")

    OUT_JSON.write_text(json.dumps(results, indent=2) + "\n")
    log(f"wrote {OUT_JSON}")
    # Console summary
    print("\n=== VERDICTS ===")
    for v in results["verdicts"]:
        print(f"  {v['fixture']:16s} {v['lighting']:9s} "
              f"base={v['baseline_median_ms']:6.2f} "
              f"native={v['all_native_median_ms']:6.2f} "
              f"Δ={v['delta_ms']:+6.2f}  {v['verdict']}")


if __name__ == "__main__":
    main()
