#!/usr/bin/env python3
"""RT A3a — per-term native-resolution frame-cost measurement (split dispatch).

Measures steady-state content frame time after the A3a split-dispatch change:
- Split-baseline: both dispatches at half-res (same as pre-split baseline)
- Per-term native: each term nativizes its own dispatch (shadow→mask, ao/gi/reflection→lighting)
- All-native: both dispatches at native resolution

ARCHITECTURE NOTE (A3a split):
  The single shared dispatch is now TWO dispatches of the SAME kernel:
  - Dispatch M (mask): shadow visibility only → rt_mask_half at its own resolution
  - Dispatch L (lighting): AO + GI + reflection → rt_irr_half, rt_refl_half, rt_normal_half at their own resolution
  Term→dispatch mapping by actual output writes:
  - shadow → mask dispatch (writes out_sv)
  - ao → lighting dispatch (writes out_irr.a)
  - gi → lighting dispatch (writes out_irr.rgb)
  - reflection → lighting dispatch (writes out_refl)

Protocol:
  For each fixture x lighting x {split-baseline, shadow, ao, gi, reflection, all-native}:
    manifold rt-capture <fixture> --width 3840 --height 2160 --frames 150 \
        --sync-gpu --set-at 10 <lighting snaps>
    with MANIFOLD_RT_NATIVE_TERMS set per config.
  Discard the first 30 frames (warmup: accel build + JIT), take the last 120.
  Report median + p95 GPU ms.

Output: scripts/rt_a3_term_cost.json — per fixture x lighting x config ->
median_ms, p95_ms, delta_vs_split_baseline_ms, fits_41_6ms; plus verdicts.
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
OUT_JSON = SLOT / "scripts/rt_a3_term_cost.json"

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

# A3a split-dispatch configs: per-term and combinations
CONFIGS = {
    "split-baseline":  "",                  # both half-res (default)
    "shadow":        "shadow",             # mask dispatch native
    "ao":            "ao",                 # lighting dispatch native
    "gi":            "gi",                 # lighting dispatch native
    "reflection":    "reflection",         # lighting dispatch native
    "all-native":    "shadow,ao,gi,reflection",  # both native
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
    # RETIRED (RT_QUALITY_SETTINGS_DESIGN.md D8): the renderer no longer reads
    # MANIFOLD_RT_NATIVE_TERMS — this assignment is inert. Ray resolution is a
    # per-project setting (ProjectSettings.rt_quality) now.
    if native_terms:
        log("WARNING: MANIFOLD_RT_NATIVE_TERMS is retired (inert) — set rt_quality in the fixture instead")
    env["MANIFOLD_RT_NATIVE_TERMS"] = native_terms
    # Unique capture dir per run so two runs never interleave their PNGs.
    env["MANIFOLD_RT_CAPTURE_DIR"] = f"/tmp/rt_a3_{os.getpid()}_{int(time.time()*1000)}"

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
        return {}, "TIMEOUT after 240s", False
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
            "A3a split-dispatch: mask (shadow) and lighting (AO/GI/reflection) run "
            "as separate dispatches at their own resolutions. Term→dispatch mapping: "
            "shadow→mask, ao→lighting, gi→lighting, reflection→lighting. Each dispatch "
            "runs at native resolution when any of its terms is listed in "
            "MANIFOLD_RT_NATIVE_TERMS. Split-baseline (both half-res) should match "
            "pre-split baseline; any difference is split overhead."
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

            # Calculate deltas against split-baseline for each config
            if "split-baseline" in per:
                base = per["split-baseline"]
                for cfg_name, native_median in per.items():
                    if cfg_name == "split-baseline":
                        continue
                    delta = native_median - base
                    fits = native_median <= FRAME_BUDGET_MS
                    # Find the measurement record and add delta
                    for m in reversed(results["measurements"]):
                        if (m.get("fixture") == fx_name and m.get("lighting") == light_name
                                and m.get("config") == cfg_name):
                            m["delta_vs_split_baseline_ms"] = round(delta, 2)
                            m["fits_41_6ms"] = fits
                            break

                    # Verdict for each term
                    if cfg_name != "all-native":  # individual terms
                        verdict = ("FITS" if fits else "EXCEEDS") + f" budget (+{delta:.2f}ms)"
                        results["verdicts"].append({
                            "fixture": fx_name, "lighting": light_name,
                            "config": cfg_name,
                            "split_baseline_median_ms": round(base, 2),
                            "term_native_median_ms": round(native_median, 2),
                            "delta_ms": round(delta, 2),
                            "fits_41_6ms": fits,
                            "verdict": verdict,
                        })
                        log(f"  -> {cfg_name}: +{delta:.2f}ms {verdict}")

                # Special verdict for split overhead (split-baseline vs historical baseline)
                # and all-native combination
                if "all-native" in per:
                    all_native = per["all-native"]
                    all_delta = all_native - base
                    all_fits = all_native <= FRAME_BUDGET_MS
                    verdict = ("FITS" if all_fits else "EXCEEDS") + f" budget (+{all_delta:.2f}ms)"
                    results["verdicts"].append({
                        "fixture": fx_name, "lighting": light_name,
                        "config": "all-native",
                        "split_baseline_median_ms": round(base, 2),
                        "all_native_median_ms": round(all_native, 2),
                        "delta_ms": round(all_delta, 2),
                        "fits_41_6ms": all_fits,
                        "verdict": verdict,
                    })
                    log(f"  -> all-native: +{all_delta:.2f}ms {verdict}")

    OUT_JSON.write_text(json.dumps(results, indent=2) + "\n")
    log(f"wrote {OUT_JSON}")
    # Console summary
    print("\n=== VERDICTS ===")
    for v in results["verdicts"]:
        cfg = v['config'].ljust(12)
        print(f"  {v['fixture']:16s} {v['lighting']:9s} "
              f"{cfg}  "
              f"base={v['split_baseline_median_ms']:6.2f} "
              f"native={v['term_native_median_ms']:6.2f} "
              f"Δ={v['delta_ms']:+6.2f}  {v['verdict']}")


if __name__ == "__main__":
    main()
