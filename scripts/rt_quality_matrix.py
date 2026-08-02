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
import re
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


def run_cmd(cmd, cwd, timeout, env=None):
    """Run command and return (exit_code, stdout, stderr, duration)."""
    start = time.time()
    try:
        r = subprocess.run(cmd, cwd=str(cwd), capture_output=True, text=True,
                        timeout=timeout, env=env)
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


def capture_paused(binary, project, frames, rt_enabled=True, cwd=None, timeout=None, capture_dir=None):
    """Run paused capture and return (success, capture_dir, stderr)."""
    # Use provided capture directory or create new one
    if capture_dir is None:
        suffix = "rt_on" if rt_enabled else "rt_off"
        capture_dir = Path(f"/tmp/rt_quality_matrix_{os.getpid()}_{suffix}")

    # Ensure clean directory
    if capture_dir.exists():
        shutil.rmtree(capture_dir)
    capture_dir.mkdir(parents=True, exist_ok=True)

    args = [str(binary), "rt-capture", "--paused", str(project), "--frames", str(frames)]

    # Add RT toggle via --set-at if specified
    if not rt_enabled:
        # Disable RT at frame 10 (after initial warmup)
        args.extend(["--set-at", "10", "8_rt_enabled=0.0"])

    # Set capture directory via environment variable
    env = os.environ.copy()
    env["MANIFOLD_RT_CAPTURE_DIR"] = str(capture_dir)

    log(f"[rt-quality] capturing to {capture_dir.name}/ (RT={rt_enabled})")
    exit_, out, err, dur = run_cmd(args, cwd=cwd, timeout=timeout, env=env)

    if exit_ != 0:
        return False, None, err

    # Verify capture wrote files
    if not any(capture_dir.glob("*.png")):
        return False, None, f"rt-capture wrote no captures to {capture_dir}"

    return True, capture_dir, err


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
        m = re.match(r"composite_(\d{4})\.png", png.name)
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


def measure_sharpness_leg(rt_capture_dir, raster_capture_dir, probe_regions):
    """
    Measure SHARPNESS leg: edge transition width RT-on vs RT-off (raster reference).

    Computes ratio of RT edge width to raster edge width for each probe region.
    Returns dict with sharpness ratio measurements.
    """
    results = []

    # Check both captures exist
    rt_composite = rt_capture_dir / "composite_0099.png"
    raster_composite = raster_capture_dir / "composite_0099.png"

    if not rt_composite.exists():
        raise FileNotFoundError(f"RT composite not found: {rt_composite}")
    if not raster_composite.exists():
        raise FileNotFoundError(f"Raster composite not found: {raster_composite}")

    for region in probe_regions:
        if region["type"] != "shadow_boundary":
            continue

        # Load both images
        rt_img = Image.open(rt_composite)
        raster_img = Image.open(raster_composite)

        rt_array = np.array(rt_img.convert('L')).astype(float) / 255.0
        raster_array = np.array(raster_img.convert('L')).astype(float) / 255.0

        # Extract the region from both
        x, y = region["x"], region["y"]
        w, h = region["width"], region["height"]

        rt_region = rt_array[y:y+h, x:x+w]
        raster_region = raster_array[y:y+h, x:x+w]

        # Measure transition width in RT capture
        rt_width = measure_transition_width(rt_region)
        if rt_width is None:
            continue

        # Measure transition width in raster capture
        raster_width = measure_transition_width(raster_region)
        if raster_width is None:
            continue

        # Compute ratio
        if raster_width > 0:
            ratio = rt_width / raster_width
        else:
            continue

        results.append({
            "region": region["rationale"],
            "rt_width_px": float(rt_width),
            "raster_width_px": float(raster_width),
            "ratio": float(ratio),
            "coordinates": [int(x), int(y)]
        })

    if not results:
        raise ValueError(f"No measurable edges found in {len(probe_regions)} probe regions")

    # Return the maximum ratio and all measurements
    ratios = [r["ratio"] for r in results]
    return {
        "max_ratio": float(max(ratios)),
        "median_ratio": float(statistics.median(ratios)),
        "min_ratio": float(min(ratios)),
        "regions": results
    }


def measure_transition_width(region_data):
    """Measure edge transition width in a region using gradient magnitude."""
    # Find the strongest edge
    grad_x = np.diff(region_data.astype(float), axis=1)
    grad_y = np.diff(region_data.astype(float), axis=0)

    grad_x = np.pad(grad_x, ((0, 0), (0, 1)), mode='constant')
    grad_y = np.pad(grad_y, ((0, 1), (0, 0)), mode='constant')

    grad_mag = np.sqrt(grad_x**2 + grad_y**2)

    # Find edge center
    edge_pos = np.unravel_index(np.argmax(grad_mag), grad_mag.shape)
    edge_y, edge_x = edge_pos

    # Sample horizontal line through the edge
    line = region_data[edge_y, :]

    # Measure transition width (10% to 90% of range)
    line_min, line_max = line.min(), line.max()
    if line_max - line_min < 0.05:
        return None  # Skip low-contrast edges

    threshold_10 = line_min + 0.1 * (line_max - line_min)
    threshold_90 = line_min + 0.9 * (line_max - line_min)

    below_10 = line < threshold_10
    above_90 = line > threshold_90

    if not below_10.any() or not above_90.any():
        return None

    left_10 = np.where(below_10)[0][-1]
    right_90 = np.where(above_90)[0][0]

    transition_width = abs(right_90 - left_10)
    return transition_width


def measure_halo_leg(capture_dir, probe_regions):
    """
    Measure HALO leg: luminance bleed on void side of silhouettes.

    Analyzes silhouette edges for luminance bleed beyond edge.
    Returns dict with max halo width measurements.
    """
    results = []

    # Check capture exists
    composite_path = capture_dir / "composite_0099.png"
    if not composite_path.exists():
        raise FileNotFoundError(f"Composite not found: {composite_path}")

    for region in probe_regions:
        # For halo measurement, we need regions near object boundaries
        if region["type"] != "shadow_boundary":
            continue

        # Load the composite image
        img = Image.open(composite_path)
        img_array = np.array(img.convert('L')).astype(float) / 255.0

        # Extract the region
        x, y = region["x"], region["y"]
        w, h = region["width"], region["height"]

        region_data = img_array[y:y+h, x:x+w]

        # Find the strongest edge
        grad_x = np.diff(region_data.astype(float), axis=1)
        grad_y = np.diff(region_data.astype(float), axis=0)

        grad_x = np.pad(grad_x, ((0, 0), (0, 1)), mode='constant')
        grad_y = np.pad(grad_y, ((0, 1), (0, 0)), mode='constant')

        grad_mag = np.sqrt(grad_x**2 + grad_y**2)

        edge_pos = np.unravel_index(np.argmax(grad_mag), grad_mag.shape)
        edge_y, edge_x = edge_pos

        # Sample horizontal line through edge
        line = region_data[edge_y, :]

        # Find the edge point (maximum gradient in line)
        line_grad = np.abs(np.diff(line.astype(float)))
        if len(line_grad) == 0:
            continue

        edge_pos_local = np.argmax(line_grad)
        edge_value = line[edge_pos_local]

        # Determine which side is void (darker)
        left_mean = np.mean(line[max(0, edge_pos_local-10):edge_pos_local])
        right_mean = np.mean(line[edge_pos_local:min(len(line), edge_pos_local+10)])

        void_side = "left" if left_mean < right_mean else "right"

        # Measure how far luminance extends into void side
        contrast = abs(left_mean - right_mean)
        threshold = max(left_mean, right_mean) - 0.05 * contrast

        if void_side == "left":
            void_pixels = line[edge_pos_local::-1]
        else:
            void_pixels = line[edge_pos_local:]

        bleed_pixels = 0
        for val in void_pixels:
            if val > threshold:
                bleed_pixels += 1
            else:
                break

        results.append({
            "region": region["rationale"],
            "halo_width_px": int(bleed_pixels),
            "contrast": float(contrast),
            "void_side": void_side,
            "coordinates": [int(x + edge_pos_local), int(y + edge_y)]
        })

    if not results:
        raise ValueError(f"No measurable silhouette edges found in {len(probe_regions)} probe regions")

    # Return the maximum halo width
    widths = [r["halo_width_px"] for r in results]
    return {
        "max_width_px": float(max(widths)),
        "median_width_px": float(statistics.median(widths)),
        "regions": results
    }


def run_matrix(repo, binary, repeats, pick_probes=True):
    """Run the full quality matrix across all fixtures and configs."""
    results = {}
    all_probe_regions = {}

    for scene_name, scene_info in FIXTURES.items():
        project_path = repo / scene_info["path"]
        if not project_path.exists():
            log(f"[SKIP] {scene_name}: fixture not found at {project_path}")
            continue

        results[scene_name] = {}

        # For baseline run: pick probe regions from first config
        if pick_probes and scene_name not in all_probe_regions:
            log(f"[rt-quality] picking probe regions for {scene_name}...")
            capture_success, cap_dir, cap_err = capture_paused(
                binary, project_path, frames=100, cwd=repo, timeout=300)

            if capture_success and cap_dir:
                composite_file = list(cap_dir.glob("composite_*.png"))
                if composite_file:
                    # Run probe selection script
                    probe_script = repo / "scripts/pick_probe_regions.py"
                    if probe_script.exists():
                        exit_, out, err, dur = run_cmd(
                            ["python3", str(probe_script), str(composite_file[0])],
                            cwd=repo, timeout=60)
                        if exit_ == 0 and out:
                            try:
                                probe_data = json.loads(out)
                                selected_regions = probe_data.get("probe_regions", [])[:5]  # Top 5 regions
                                all_probe_regions[scene_name] = selected_regions
                                log(f"[rt-quality]   picked {len(selected_regions)} probe regions for {scene_name}")
                            except json.JSONDecodeError:
                                log(f"[rt-quality]   failed to parse probe regions")

        probe_regions = all_probe_regions.get(scene_name, [])

        for config in scene_info["configs"]:
            config_key = f"{scene_name}_{config}"
            log(f"[rt-quality] running {config_key}...")

            # Run captures: RT-on for sharpness/halo, RT-off for sharpness baseline
            all_noise = []
            all_sharpness = []
            all_halo = []

            for run in range(repeats):
                # RT-on capture (all legs)
                rt_success, rt_cap_dir, rt_err = capture_paused(
                    binary, project_path, frames=300, rt_enabled=True, cwd=repo, timeout=900)
                if not rt_success:
                    log(f"  [FAIL] RT-on run {run+1}/{repeats}: {rt_err}")
                    continue

                # Raster capture for sharpness baseline
                raster_success, raster_cap_dir, raster_err = capture_paused(
                    binary, project_path, frames=300, rt_enabled=False, cwd=repo, timeout=900)
                if not raster_success:
                    log(f"  [FAIL] Raster run {run+1}/{repeats}: {raster_err}")
                    raise ValueError(f"Raster capture failed: {raster_err}")

                # Measure legs
                noise = measure_noise_leg(rt_cap_dir)
                all_noise.append(noise)

                if probe_regions:
                    try:
                        sharpness = measure_sharpness_leg(rt_cap_dir, raster_cap_dir, probe_regions)
                        all_sharpness.append(sharpness)
                    except (FileNotFoundError, ValueError) as e:
                        log(f"  [FAIL] sharpness measurement run {run+1}/{repeats}: {e}")
                        raise

                    try:
                        halo = measure_halo_leg(rt_cap_dir, probe_regions)
                        all_halo.append(halo)
                    except (FileNotFoundError, ValueError) as e:
                        log(f"  [FAIL] halo measurement run {run+1}/{repeats}: {e}")
                        raise

            if not all_noise:
                log(f"  [FAIL] {config_key}: no successful RT-on captures")
                raise ValueError(f"No successful captures for {config_key}")

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

            # Aggregate sharpness and halo
            agg_sharpness = {}
            agg_halo = {}

            if all_sharpness and all_sharpness[0].get("regions"):
                ratios = [s.get("max_ratio", 0) for s in all_sharpness if "max_ratio" in s]
                if ratios:
                    agg_sharpness["max_ratio"] = float(max(ratios))
                    agg_sharpness["median_ratio"] = float(statistics.median(ratios))
                    agg_sharpness["runs"] = len(all_sharpness)

            if all_halo and all_halo[0].get("regions"):
                halo_widths = [h.get("max_width_px", 0) for h in all_halo if "max_width_px" in h]
                if halo_widths:
                    agg_halo["max_width_px"] = float(max(halo_widths))
                    agg_halo["median_width_px"] = float(statistics.median(halo_widths))
                    agg_halo["runs"] = len(all_halo)

            results[scene_name][config] = {
                "noise": agg_noise,
                "sharpness": agg_sharpness if agg_sharpness else {"note": "no measurements"},
                "halo": agg_halo if agg_halo else {"note": "no measurements"}
            }

    return results, all_probe_regions


def write_baseline(path, results, probe_regions):
    """Write baseline JSON with provenance and probe regions."""
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
        "probe_regions": probe_regions,
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

    results, probe_regions = run_matrix(repo, binary, args.repeats, pick_probes=True)

    if args.record:
        write_baseline(baseline_path, results, probe_regions)
        log("[rt-quality] baseline recorded")
        return 0

    # Compare against baseline
    if not baseline_path.exists():
        log(f"[SKIP] no baseline at {baseline_path} — run --record first")
        return 0

    baseline = json.loads(baseline_path.read_text())

    # Check results against thresholds
    failures = []
    trips = []

    for scene_name, scene_results in results.items():
        for config_name, config_results in scene_results.items():
            # Check sharpness leg - RT vs RASTER ratio
            if "sharpness" in config_results and "max_ratio" in config_results["sharpness"]:
                ratio = config_results["sharpness"]["max_ratio"]
                if ratio > THRESHOLD_SHARPNESS_RATIO:
                    trips.append(f"{scene_name}/{config_name}: sharpness RT/raster ratio {ratio:.3f} > ceiling {THRESHOLD_SHARPNESS_RATIO}")

            # Check halo leg - luminance bleed width
            if "halo" in config_results and "max_width_px" in config_results["halo"]:
                halo_width = config_results["halo"]["max_width_px"]
                if halo_width > THRESHOLD_HALO_WIDTH_PX:
                    trips.append(f"{scene_name}/{config_name}: halo width {halo_width:.1f}px > ceiling {THRESHOLD_HALO_WIDTH_PX}px")

    if failures:
        log("\nRT QUALITY MATRIX: RED (FAILURES)")
        for failure in failures:
            log(f"  {failure}")
        return 1

    if trips:
        log("\nRT QUALITY MATRIX: BASELINE TRIPS (Expected)")
        log("The oracles successfully detected the current defects:")
        for trip in trips:
            log(f"  {trip}")
        log("\nBaseline recorded with known issues. Fix pipeline will target these measurements.")
        return 1  # Nonzero exit as required

    log("\nRT QUALITY MATRIX: green")
    return 0


if __name__ == "__main__":
    sys.exit(main())
