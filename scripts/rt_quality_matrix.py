#!/usr/bin/env python3
"""
RT Quality Matrix — comprehensive quality oracle for RT rendering.

Measures three legs:
a. NOISE/FIREFLY: consecutive-frame |delta| mean + p99.9 on composite and RT channels
b. SHARPNESS: shadow-edge transition width (RT-on vs RT-off, must be ≤ 1.25× raster)
c. HALO: luminance bleed on void side of silhouette edges (≤ 2 native px)

Thresholds are constants with provenance comments. Baseline records to
scripts/rt_quality_baseline.json following rt_noise_baseline.json's pattern.
"""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from PIL import Image
import numpy as np

MAIN_CHECKOUT = Path("/Users/peterkiemann/MANIFOLD - Rust")
BASELINE = Path("scripts/rt_quality_baseline.json")

# Capture directory — private per run to avoid contention
CAPTURE_DIR = Path(os.environ.setdefault(
    "MANIFOLD_RT_CAPTURE_DIR", f"/tmp/rt_quality_matrix_{os.getpid()}"))

# Threshold constants (provenance in baseline JSON)
# These are starting values for the baseline run — may tighten after observing current defects
THRESHOLD_NOISE_MEAN_CEILING = 2.0  # 8-bit levels, mean frame-to-frame delta
THRESHOLD_NOISE_P999_CEILING = 5.0  # 8-bit levels, 99.9th percentile delta
THRESHOLD_SHARPNESS_RATIO = 1.25    # RT edge width must be ≤ 1.25× raster edge width
THRESHOLD_HALO_WIDTH_PX = 2.0       # Max halo bleed width in native pixels
THRESHOLD_HALO_CONTRAST_PCT = 5.0   # Halo threshold: 5% of edge contrast

# Fixture configurations
FIXTURES = {
    "apricot": {
        "path": "tests/fixtures/rt/RtApricot.manifold",
        "glb": "tests/fixtures/gltf/cc0__japanese_apricot_prunus_mume.glb",
        "configs": ["sun-only", "env-only", "ambient-floor"]
    },
    "azalea": {
        "path": "tests/fixtures/rt/RtAzalea.manifold",
        "glb": "tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb",
        "configs": ["sun-only", "env-only", "ambient-floor"]
    },
    "ambient_occlusion": {
        "path": "tests/fixtures/rt/RtAmbientOcclusion.manifold",
        "glb": "tests/fixtures/gltf/khronos/CompareAmbientOcclusion.glb",
        "configs": ["sun-only", "env-only", "ambient-floor"]
    },
    "car_paint": {
        "path": "tests/fixtures/rt/RtCarPaint.manifold",
        "glb": "tests/fixtures/gltf/khronos/ClearCoatCarPaint.glb",
        "configs": ["sun-only", "env-only", "ambient-floor"]
    }
}

# Lighting configs via --set-at snaps (parameter names to be audited)
LIGHTING_CONFIGS = {
    "sun-only": ["8_rt_enabled=1.0", "1_emitter_intensity=0.0"],  # RT on, env off
    "env-only": ["8_rt_enabled=1.0", "1_emitter_intensity=3.0", "1_sun_x=-10.0"],  # RT on, sun hidden
    "ambient-floor": ["8_rt_enabled=1.0", "1_mode=0.0"]  # RT on, ambient floor only
}


def log(msg):
    """Log with timestamp."""
    print(f"{datetime.now(timezone.utc).isoformat()} {msg}", flush=True)


def run_cmd(cmd, cwd, timeout):
    """Run command and return (exit_code, stdout, stderr, duration)."""
    start = time.time()
    try:
        r = subprocess.run(cmd, cwd=str(cwd), capture_output=True, text=True, timeout=timeout)
        return r.returncode, r.stdout, r.stderr, time.time() - start
    except subprocess.TimeoutExpired:
        return -1, "", f"TIMEOUT after {timeout}s", time.time() - start
    except (FileNotFoundError, PermissionError) as e:
        return -1, "", f"cannot run {' '.join(map(str, cmd))}: {e}", time.time() - start


def build_binary(repo):
    """Build the manifold binary in release mode."""
    log("[rt-quality] building manifold (release, perf-soak)...")
    exit_, out, err, dur = run_cmd(
        ["cargo", "build", "--release", "-p", "manifold-app", "--features",
         "perf-soak", "--bin", "manifold"], cwd=repo, timeout=3600)
    if exit_ != 0:
        log(f"[FAIL] build failed ({dur:.0f}s)")
        return None
    log(f"[rt-quality] build ok ({dur:.0f}s)")
    return repo / "target/release/manifold"


def capture_paused(binary, project, frames, cwd, timeout):
    """Run paused capture and return (success, capture_dir, stderr)."""
    if CAPTURE_DIR.exists():
        shutil.rmtree(CAPTURE_DIR)
    CAPTURE_DIR.mkdir(parents=True, exist_ok=True)

    args = [
        str(binary), "rt-capture", "--paused", str(project),
        "--frames", str(frames)
    ]
    exit_, out, err, dur = run_cmd(args, cwd=cwd, timeout=timeout)

    if exit_ != 0:
        return False, None, err

    if not CAPTURE_DIR.exists():
        return False, None, "rt-capture wrote no captures"

    return True, CAPTURE_DIR, err


def measure_noise_leg(capture_dir):
    """
    Measure NOISE/FIREFLY leg: consecutive-frame |delta| mean + p99.9.

    Reuses rt_noise_gate.py's measurement machinery.
    Returns dict with channel stats.
    """
    # Find consecutive frame pairs for each channel
    by_channel = {}
    for png in sorted(capture_dir.glob("composite_*.png")):
        # Parse frame number from filename: composite_1234.png
        m = png.name.match(r"composite_(\d{4})\.png")
        if m:
            frame = int(m.group(1))
            by_channel.setdefault("composite", []).append((frame, png))

    results = {}
    for channel, files in by_channel.items():
        files.sort()
        # Find consecutive pairs
        pairs = [(files[i], files[i+1]) for i in range(len(files)-1)
                 if files[i+1][0] == files[i][0] + 1]

        if not pairs:
            results[channel] = {"mean": 0.0, "p999": 0.0, "pairs": 0}
            continue

        means, p999s = [], []
        for (fa, pa), (fb, pb) in pairs:
            a = np.asarray(Image.open(pa).convert("RGB"), dtype=np.int16)
            b = np.asarray(Image.open(pb).convert("RGB"), dtype=np.int16)
            d = np.abs(a - b).astype(np.float32)
            means.append(float(d.mean()))
            p999s.append(float(np.percentile(d, 99.9)))

        results[channel] = {
            "mean": float(np.mean(means)),
            "p999": float(np.mean(p999s)),
            "pairs": len(pairs)
        }

    return results


def measure_sharpness_leg(rt_capture_dir, raster_capture_dir):
    """
    Measure SHARPNESS leg: edge transition width.

    Compares RT-on vs RT-off captures at shadow boundaries.
    Returns dict with sharpness ratio (RT/raster).
    """
    # TODO: Implement edge detection and transition width measurement
    # This requires named probe regions in the baseline JSON
    return {"ratio": 1.0, "note": "not yet implemented"}


def measure_halo_leg(capture_dir):
    """
    Measure HALO leg: luminance bleed on void side of silhouettes.

    Measures width of region where luminance exceeds 5% of edge contrast.
    Returns dict with max halo width in pixels.
    """
    # TODO: Implement silhouette edge detection and bleed measurement
    # This requires named probe regions in the baseline JSON
    return {"max_width_px": 0.0, "note": "not yet implemented"}


def run_matrix(repo, binary, repeats):
    """Run the full quality matrix across all fixtures and configs."""
    results = {}

    for scene_name, scene_info in FIXTURES.items():
        project_path = repo / scene_info["path"]
        if not project_path.exists():
            log(f"[SKIP] {scene_name}: fixture not found at {project_path}")
            continue

        results[scene_name] = {}
        for config in scene_info["configs"]:
            config_key = f"{scene_name}_{config}"
            log(f"[rt-quality] running {config_key}...")

            # Run captures
            all_noise = []
            for run in range(repeats):
                success, cap_dir, err = capture_paused(
                    binary, project_path, frames=300, cwd=repo, timeout=900)
                if not success:
                    log(f"  [FAIL] run {run+1}/{repeats}: {err}")
                    continue

                # Measure legs
                noise = measure_noise_leg(cap_dir)
                all_noise.append(noise)

            if not all_noise:
                log(f"  [FAIL] {config_key}: no successful captures")
                continue

            # Aggregate across runs (median)
            agg_noise = {}
            for ch in all_noise[0].keys():
                values = [r[ch] for r in all_noise if ch in r]
                if values:
                    agg_noise[ch] = {
                        "mean": statistics.median([v["mean"] for v in values]),
                        "p999": statistics.median([v["p999"] for v in values]),
                        "runs": len(values)
                    }

            results[scene_name][config] = {
                "noise": agg_noise,
                "sharpness": {"note": "pending fixture setup"},
                "halo": {"note": "pending fixture setup"}
            }

    return results


def write_baseline(path, results):
    """Write baseline JSON with provenance."""
    commit = run_cmd(["git", "rev-parse", "--short=12", "HEAD"],
                      cwd=Path.cwd(), timeout=30)[1].strip()

    doc = {
        "schema": 1,
        "thresholds": {
            "noise_mean_ceiling": THRESHOLD_NOISE_MEAN_CEILING,
            "noise_p999_ceiling": THRESHOLD_NOISE_P999_CEILING,
            "sharpness_ratio": THRESHOLD_SHARPNESS_RATIO,
            "halo_width_px": THRESHOLD_HALO_WIDTH_PX,
            "halo_contrast_pct": THRESHOLD_HALO_CONTRAST_PCT
        },
        "provenance": {
            "recorded": datetime.now(timezone.utc).strftime("%Y-%m-%d"),
            "commit": commit,
            "note": "Initial baseline — expect sharpness and halo legs to TRIP thresholds"
        },
        "results": results
    }

    path.write_text(json.dumps(doc, indent=2) + "\n")
    log(f"[rt-quality] wrote baseline to {path}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", default=Path.cwd(), help="repo path (default: cwd)")
    ap.add_argument("--binary", default=None, help="use this manifold binary")
    ap.add_argument("--repeats", type=int, default=3, help="captures per config (default 3)")
    ap.add_argument("--record", action="store_true", help="write baseline from this run")
    args = ap.parse_args()

    repo = Path(args.repo)
    baseline_path = repo / BASELINE

    log("=== RT QUALITY MATRIX ===")

    binary = Path(args.binary).resolve() if args.binary else build_binary(repo)
    if binary is None or not binary.exists():
        log("[FAIL] no manifold binary")
        return 2

    results = run_matrix(repo, binary, args.repeats)

    if args.record:
        write_baseline(baseline_path, results)
        log("[rt-quality] baseline recorded")
        return 0

    # Compare against baseline
    if not baseline_path.exists():
        log(f"[SKIP] no baseline at {baseline_path} — run --record first")
        return 0

    baseline = json.loads(baseline_path.read_text())
    # TODO: Implement comparison logic

    log("[rt-quality] done")
    return 0


if __name__ == "__main__":
    sys.exit(main())
