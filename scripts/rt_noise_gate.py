#!/usr/bin/env python3
"""RT temporal-stability gate — frame-to-frame flicker on a static scene, as a number.

The show question: a paused shot of a converged scene should hold still. When
RT accumulation regresses, it boils — pixels crawl between consecutive frames
even though nothing moves. That was cut 11x on 2026-07-30 by measuring, and
this gate is what stops the next change from quietly giving it back.

WHAT IT MEASURES
`manifold rt-capture --paused` plays a real project, pauses, keeps ticking, and
dumps a run of CONSECUTIVE frames deep into the paused phase (consecutive is
the whole point — sparse captures cannot see frame-to-frame variation). For
each RT channel this gate takes the per-pixel |delta| between consecutive frame
pairs and reports mean / 99.9th percentile / max in 8-bit levels. Mean catches
broad boil; p99.9 catches localised fireflies; max is reported, never gated —
one pixel is not a verdict.

TWO LEGS: STABILITY + CORRECTNESS (ED7, RAYTRACING_DESIGN.md section 14)
Stability alone cannot certify — the frozen-seed trap (BUG-ipad): a change that
freezes any RT randomness drives every gated statistic to ~0 while destroying
the estimate (a fixed N-sample pattern is not convergence). Green therefore
requires BOTH:
1. Stability — the frame-to-frame ceilings below.
2. Correctness — the RT furnace oracle
   (`crates/manifold-renderer/tests/gpu_proofs/rt_furnace_oracle.rs`, run via
   `cargo test -p manifold-renderer --features gpu-proofs --test gpu_proofs
   -- rt_furnace`): a flat albedo-1 surface under a closed-form uniform
   environment must read back the field radiance on the TRACED path (RT on)
   and match the raster path (RT off) within tolerance (I-ED4's brightness
   leg, which since ED-A passes through the env+GI gather, not the irradiance
   map); a floor-wall corner must read darker than the open floor (I-ED4's
   corner leg); and the sun-disc firefly fixture must stay under its firefly
   ceiling while preserving the sun's energy (the ED5 clamp's tuned regime).
   The furnace is the oracle the noise gate's image-less numbers cannot be
   — every verdict in BOTH legs is a computed number against a committed
   ceiling; the PNGs are just the transport for pixel values.

WHY IT ALSO CHECKS FOR SIGNAL
A dead channel is perfectly stable, so the gate refuses to certify one. Seven of
ten runs on 2026-07-30 read every RT channel at exactly zero with the visibility
mask pinned at 255 while the composite still rendered — which turned out to be
capture-directory contention, not an RT defect (BUG-mw0x — concurrent rt-capture
runs clobbered each other via a shared /tmp dir). The lesson survives its cause:
a delta-only gate called that state calm. Each channel carries a
`min_signal_level` floor, and a channel below its floor FAILS as inert rather
than passing as stable. The furnace leg is the second half of that same lesson
applied to correctness: a frozen channel is not a converged one.

FLAKE CONTROL
Two mechanisms, because a flaky gate gets ignored, which is worse than no gate.
1. Contamination rejection. An async accel-structure rebuild landing inside (or
   just before) the measured window resets every accumulator and reads an order
   of magnitude high. `render_scene` logs "RT accel structure (re)build
   enqueued"; a run with one of those in or near the window is DISCARDED and
   retried, never failed on.
2. Median of repeats. The verdict is the per-channel median across N clean
   runs, so one unlucky run cannot turn the gate red.

CEILINGS ARE DATA, NOT CODE
scripts/rt_noise_baseline.json holds the ceilings plus the provenance of the
run that produced them. Regenerate with `--record` after a legitimate RT
improvement, and commit the JSON — that is the whole re-baseline procedure.
While the baseline is marked unvalidated the gate SKIPS green and says so,
so wiring it into a nightly sweep cannot produce bead noise before the
ceilings mean anything.

WHERE IT RUNS
Nightly on main via scripts/trunk_health.py, and on demand for anyone touching
the RT path. NOT in the default test suite and NOT in landing_gate.py: it costs
an app build plus a multi-minute 300-frame render, and per-iteration cost that
high is exactly why the gpu-proofs gate is already opt-in.

USAGE
  scripts/rt_noise_gate.py                     # gate: build, run, compare
  scripts/rt_noise_gate.py --record            # regenerate the ceilings
  scripts/rt_noise_gate.py --repeats 5 --json  # spread evidence
  scripts/rt_noise_gate.py --skip-furnace      # stability leg only (recording,
                                               # or isolating a stability issue)

EXIT CODES
  0  green, or a loud SKIP (fixture absent, ceilings unvalidated)
  1  the furnace oracle failed, or a channel exceeded its ceiling / read inert
  2  the measurement could not be made (build failed, no consecutive pairs,
     every repeat contaminated) — an unknown answer, never a green one
"""

import argparse
import fcntl
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

MAIN_CHECKOUT = Path("/Users/peterkiemann/MANIFOLD - Rust")
BASELINE = Path("scripts/rt_noise_baseline.json")

# rt_capture.rs hardcodes its dump directory. Two concurrent captures would
# interleave PNGs into one pile and silently corrupt both metrics, so runs
# serialise on a lock instead of racing.
# A PRIVATE capture directory per gate process, exported so the harness writes
# there instead of the shared default. The lock below stops two gate runs from
# racing, but nothing stops a human or another agent running `rt-capture` by
# hand at the same time — and the harness used to clear a fixed shared path on
# entry, so an overlapping run silently destroyed the other's frames. That
# manufactured a phantom "every RT channel is zero" report (BUG-mw0x) when three
# sessions captured in parallel. Isolation beats remembering not to overlap.
CAPTURE_DIR = Path(os.environ.setdefault(
    "MANIFOLD_RT_CAPTURE_DIR", f"/tmp/rt_capture_gate_{os.getpid()}"))
LOCK_PATH = Path("/tmp/manifold-rt-noise-gate.lock")

FIXTURE_REL = Path("tests/fixtures/rt/RtNoiseTesting.manifold")

REBUILD_RE = re.compile(r"RT accel structure \(re\)build enqueued")
CAPTURE_RE = re.compile(r"\[rt-capture\] (\S+) f=(\d+) ")
PNG_RE = re.compile(r"(.+)_(\d{4})\.png$")

# An accel rebuild resets every accumulator. History is ~40 frames deep, so a
# rebuild up to this many frames before the window can still be settling
# inside it — the lookback is the history length with margin, not a guess.
CONTAMINATION_LOOKBACK_FRAMES = 60

# Ceilings need room for genuine run-to-run variance without letting a real
# regression through. Multiplicative headroom plus an absolute floor, because
# a channel measuring 0.008 needs the floor and one measuring 0.7 needs the
# multiplier.
HEADROOM = {"mean": (2.0, 0.05), "p999": (1.8, 1.0)}
# A channel is inert if it dropped to a quarter of its recorded brightness.
SIGNAL_FLOOR_FRACTION = 0.25

MIN_PAIRS = 3

# A literally black channel is not a quiet channel, it is an absent one, and its
# frame-to-frame delta of 0.0 would drag a median toward a fake-calm verdict. The
# private capture dir removed the known cause of black channels, so this guard
# should now be silent — it stays because it is the cheap check that caught that
# contamination in the first place, and a silent guard costs nothing. Such a run
# is discarded as a failed measurement; `min_signal_level` in the baseline is the
# backstop for a channel that merely dimmed.
DEAD_CHANNEL_LEVEL = 1e-6


def log(msg):
    print(msg, flush=True)


def run_cmd(cmd, cwd, timeout):
    start = time.time()
    try:
        r = subprocess.run(cmd, cwd=str(cwd), capture_output=True, text=True,
                           timeout=timeout)
    except subprocess.TimeoutExpired:
        return -1, "", f"TIMEOUT after {timeout}s: {' '.join(map(str, cmd))}", time.time() - start
    except (FileNotFoundError, PermissionError) as e:
        # A gate must always end at its own verdict line, never a traceback.
        return -1, "", f"cannot run {' '.join(map(str, cmd))}: {e}", time.time() - start
    return r.returncode, r.stdout, r.stderr, time.time() - start


# ── fixture + binary ────────────────────────────────────────────────────


def resolve_fixture(repo, explicit):
    """First existing of: --project, $MANIFOLD_RT_NOISE_PROJECT, the repo's
    fixture path, the main checkout's fixture path.

    .manifold fixtures are gitignored and copied into every worktree slot by
    agent-worktree.py, so a slot and the main checkout are both valid homes."""
    for cand in (explicit, os.environ.get("MANIFOLD_RT_NOISE_PROJECT"),
                 repo / FIXTURE_REL, MAIN_CHECKOUT / FIXTURE_REL):
        if cand and Path(cand).exists():
            return Path(cand)
    return None


def build_binary(repo):
    """Build the capture harness from the tree under test. A gate that reuses
    whatever binary happens to be lying around measures the wrong code."""
    log("[rt-noise] building manifold (release, perf-soak)...")
    # RELEASE, not debug. Two reasons, both load-bearing. A debug build is code
    # nobody ships, and this measures a timing-sensitive pipeline: an async
    # accel build races the first trace dispatch (D17), so a build several times
    # slower can land on the other side of that race. It also costs ~130s per
    # capture against ~40s, which is the difference between a gate that runs and
    # one that gets skipped.
    exit_, out, err, dur = run_cmd(
        ["cargo", "build", "--release", "-p", "manifold-app", "--features",
         "perf-soak", "--bin", "manifold"], cwd=repo, timeout=3600)
    if exit_ != 0:
        log(f"[FAIL] build failed ({dur:.0f}s)")
        for line in (out + err).rstrip().splitlines()[-20:]:
            log(f"    {line}")
        return None
    log(f"[rt-noise] build ok ({dur:.0f}s)")
    return repo / "target/release/manifold"


# ── the correctness leg (ED7) ───────────────────────────────────────────


def run_furnace_oracle(repo, timeout=1800):
    """The correctness leg: run the RT furnace oracle and report pass/fail.

    The oracle is `crates/manifold-renderer/tests/gpu_proofs/rt_furnace_oracle.rs`
    — I-ED4's brightness + corner legs, I-ED1's ambient linearity, and the
    ED-B sun-disc firefly fixture. It is a gpu-proofs test, run the same way
    `scripts/gpu_proofs_gate.py` runs GPU tests (cargo test, never nextest —
    process-per-test defeats the device lock). Debug build; the release app
    build for the stability leg is separate and happens only after this gate
    passes, so a correctness regression costs seconds, not a 300-frame render.

    Returns (passed, output)."""
    cmd = [
        "cargo", "test", "-p", "manifold-renderer", "--features", "gpu-proofs",
        "--test", "gpu_proofs", "--", "rt_furnace", "--test-threads=1",
    ]
    exit_, out, err, dur = run_cmd(cmd, cwd=repo, timeout=timeout)
    output = out + err
    # The test summary is the verdict line; the filter (`rt_furnace`) matches
    # exactly the furnace tests, so a non-zero exit with a build failure and a
    # non-zero exit with a failed assertion are both a red correctness leg.
    summary = next((l for l in output.splitlines() if "test result:" in l), None)
    if exit_ != 0:
        log(f"[rt-noise] furnace oracle FAILED after {dur:.0f}s"
            + (f" — {summary.strip()}" if summary else ""))
        return False, output
    log(f"[rt-noise] furnace oracle ok ({dur:.0f}s)"
        + (f" — {summary.strip()}" if summary else ""))
    return True, output


# ── one capture run ─────────────────────────────────────────────────────


def consecutive_run(frames):
    """The trailing maximal block of consecutive frame numbers."""
    frames = sorted(frames)
    if not frames:
        return []
    block = [frames[-1]]
    for f in reversed(frames[:-1]):
        if f == block[0] - 1:
            block.insert(0, f)
        else:
            break
    return block


def contaminated(stderr, first_window_frame):
    """True when an accel rebuild landed in or just before the measured window.

    Ordering comes from the log itself: rt_capture prints its per-channel
    capture lines in frame order, so the capture line that FOLLOWS a rebuild
    dates it — the rebuild happened no later than that frame. The check is
    deliberately conservative: a rebuild that merely COULD have landed inside
    the lookback discards the run, because a wasted rerun costs three minutes
    and a false green costs the show."""
    lines = stderr.splitlines()
    rebuilds = [i for i, l in enumerate(lines) if REBUILD_RE.search(l)]
    if not rebuilds:
        return False, None
    cutoff_frame = first_window_frame - CONTAMINATION_LOOKBACK_FRAMES
    suspect = 0
    for idx in rebuilds:
        # No capture line after a rebuild means it is past the last measured
        # frame and cannot have touched the window.
        for line in lines[idx + 1:]:
            m = CAPTURE_RE.search(line)
            if m:
                if int(m.group(2)) >= cutoff_frame:
                    suspect += 1
                break
    if suspect:
        return True, (f"{suspect} accel rebuild(s) could have landed within "
                      f"{CONTAMINATION_LOOKBACK_FRAMES} frames of the measured "
                      f"window (first window frame {first_window_frame})")
    return False, None


def capture_once(binary, project, frames, run_dir, cwd, timeout):
    """One rt-capture run. Returns (run_dir, stderr) or (None, reason)."""
    if CAPTURE_DIR.exists():
        shutil.rmtree(CAPTURE_DIR)
    exit_, out, err, dur = run_cmd(
        [str(binary), "rt-capture", "--paused", str(project), "--frames", str(frames)],
        cwd=cwd, timeout=timeout)
    if exit_ != 0:
        return None, f"rt-capture exited {exit_} ({dur:.0f}s)"
    if not CAPTURE_DIR.exists():
        return None, "rt-capture wrote no captures"
    run_dir.mkdir(parents=True, exist_ok=True)
    for png in CAPTURE_DIR.glob("*.png"):
        shutil.copy2(png, run_dir / png.name)
    (run_dir / "capture.log").write_text(out + err)
    log(f"[rt-noise]   captured in {dur:.0f}s → {run_dir}")
    return run_dir, err


# ── the metric ──────────────────────────────────────────────────────────


def measure(run_dir):
    """Per-channel frame-to-frame stats over the consecutive run.

    Returns ({channel: {mean, p999, max, level, pairs}}, window_start) in 8-bit
    levels, where `level` is the channel's own mean brightness — the
    signal-presence check. RGB only: alpha encodes hit distance, not emitted
    light. `window_start` is the first frame of the measured window, which is
    what dates a rebuild as contaminating or harmless."""
    import numpy as np
    from PIL import Image

    by_channel = {}
    for p in sorted(run_dir.glob("*.png")):
        m = PNG_RE.match(p.name)
        if m:
            by_channel.setdefault(m.group(1), []).append((int(m.group(2)), p))

    out = {}
    window_start = None
    for ch, items in sorted(by_channel.items()):
        items.sort()
        block = consecutive_run([f for f, _ in items])
        if block:
            window_start = block[0] if window_start is None else min(window_start, block[0])
        window = set(block)
        pairs = [(items[i], items[i + 1]) for i in range(len(items) - 1)
                 if items[i][0] in window and items[i + 1][0] in window
                 and items[i + 1][0] == items[i][0] + 1]
        if not pairs:
            continue
        means, p999s, maxes, levels = [], [], [], []
        for (fa, pa), (fb, pb) in pairs:
            a = np.asarray(Image.open(pa).convert("RGB"), dtype=np.int16)
            b = np.asarray(Image.open(pb).convert("RGB"), dtype=np.int16)
            d = np.abs(a - b).astype(np.float32)
            means.append(float(d.mean()))
            p999s.append(float(np.percentile(d, 99.9)))
            maxes.append(float(d.max()))
            levels.append(float(a.mean()))
        out[ch] = {"mean": float(np.mean(means)), "p999": float(np.mean(p999s)),
                   "max": float(np.mean(maxes)), "level": float(np.mean(levels)),
                   "pairs": len(pairs)}
    return out, window_start


def dead_channels(stats):
    """Channels that emitted literally nothing this run — see DEAD_CHANNEL_LEVEL."""
    return sorted(ch for ch, v in stats.items() if v["level"] <= DEAD_CHANNEL_LEVEL)


def median_across(runs):
    """Per-channel median of each statistic, plus the observed spread."""
    channels = sorted({ch for r in runs for ch in r})
    agg = {}
    for ch in channels:
        vals = [r[ch] for r in runs if ch in r]
        entry = {"runs": len(vals), "pairs": min(v["pairs"] for v in vals)}
        for stat in ("mean", "p999", "max", "level"):
            series = [v[stat] for v in vals]
            entry[stat] = statistics.median(series)
            entry[f"{stat}_min"] = min(series)
            entry[f"{stat}_max"] = max(series)
        agg[ch] = entry
    return agg


# ── verdict ─────────────────────────────────────────────────────────────


def compare(agg, baseline):
    """Returns (failures, rows) — rows are printable regardless of verdict."""
    ceilings = baseline["channels"]
    failures = []
    rows = []
    for ch in sorted(agg):
        a = agg[ch]
        c = ceilings.get(ch)
        if c is None:
            rows.append((ch, a, None, "UNTRACKED"))
            continue
        verdicts = []
        if a["level"] < c["min_signal_level"]:
            verdicts.append(f"INERT level {a['level']:.3f} < {c['min_signal_level']:.3f}")
        if a["mean"] > c["mean"]:
            verdicts.append(f"mean {a['mean']:.3f} > {c['mean']:.3f}")
        if a["p999"] > c["p999"]:
            verdicts.append(f"p99.9 {a['p999']:.2f} > {c['p999']:.2f}")
        if verdicts:
            failures.append((ch, "; ".join(verdicts)))
            rows.append((ch, a, c, "FAIL"))
        else:
            rows.append((ch, a, c, "ok"))
    missing = [ch for ch in ceilings if ch not in agg]
    for ch in missing:
        failures.append((ch, "channel absent from capture — harness changed?"))
    return failures, rows


def print_table(rows):
    log(f"\n{'channel':22} {'runs':>4} {'pairs':>5} {'mean|d|':>9} {'ceil':>7} "
        f"{'p99.9':>7} {'ceil':>7} {'max':>6} {'level':>7}")
    for ch, a, c, status in rows:
        cm = f"{c['mean']:.3f}" if c else "-"
        cp = f"{c['p999']:.1f}" if c else "-"
        mark = "" if status == "ok" else f"  <<< {status}"
        log(f"{ch:22} {a['runs']:>4} {a['pairs']:>5} {a['mean']:>9.3f} {cm:>7} "
            f"{a['p999']:>7.2f} {cp:>7} {a['max']:>6.0f} {a['level']:>7.2f}{mark}")
    log("Units: 8-bit sRGB levels (0-255) of |frame N - frame N+1|. "
        "A genuinely static channel reads ~0.0 mean.")


def print_spread(agg):
    log("\nrun-to-run spread (min … max across clean repeats):")
    for ch in sorted(agg):
        a = agg[ch]
        log(f"  {ch:22} mean {a['mean_min']:.3f} … {a['mean_max']:.3f}   "
            f"p99.9 {a['p999_min']:.2f} … {a['p999_max']:.2f}")


# ── the motion leg (BUG-sz0u) ───────────────────────────────────────────
#
# Every other leg of this gate is static (a paused scene), so motion-only
# regressions — the D-64 boil, the sv-gate re-trip (BUG-tr5o) — shipped
# invisible until Peter saw them. This leg drives the camera:
# `rt-capture --animate 5_orbit 0.02` writes a per-frame orbit ramp through
# the real MutateProject path, `--frame-clock` keeps engine time
# deterministic under slow renders, and the capture window sits deep into
# the ramp (frames 96..156, every 4th). The assertions come from the
# capture log's own channel stats, no PNG math:
#
#  - refl_raw hit must be > 0 at EVERY capture. An empty accel (BUG-oqta's
#    class) renders the whole leg calm-and-meaningless; a dead-channel
#    reading is a loud rejection, never a pass.
#  - sv_hold mean-R max: the sv gate re-trip observable that settled
#    BUG-tr5o — under a healthy motion-scaled gate the hold population
#    stays bounded; a regression re-trips and this climbs.
#  - moments center-R min/median: the GI estimate's center-crop level.
#    Motion that collapses accumulation history (the D-64 boil) drags it;
#    the floors live in the baseline's `motion` section.
#
# The leg is RED-validated against the pre-D-64 binary (52b6ec2b) — the
# boil era — before any nightly wiring; that validation is recorded in the
# baseline provenance and on the bead.

MOTION_CAPTURE_RE = re.compile(
    r"\[rt-capture\] (\S+) f=(\d+) dim=\S+ hit=([0-9.eE+-]+) luma=\S+ sd=\S+ "
    r"mean=\[([^\]]+)\] center=\[([^\]]+)\]")

MOTION_ANIMATE = ("5_orbit", "0.02")
MOTION_FRAMES = 160
MOTION_CAPTURE_EVERY = 4
MOTION_CAPTURE_FROM = 96


def motion_capture_once(binary, project, run_dir, cwd, timeout):
    """One motion-leg capture. Returns (stderr, ) or (None, reason)."""
    if CAPTURE_DIR.exists():
        shutil.rmtree(CAPTURE_DIR)
    exit_, out, err, dur = run_cmd(
        [str(binary), "rt-capture", str(project),
         "--frames", str(MOTION_FRAMES),
         "--animate", *MOTION_ANIMATE,
         "--frame-clock",
         "--capture-every", str(MOTION_CAPTURE_EVERY),
         "--capture-from", str(MOTION_CAPTURE_FROM)],
        cwd=cwd, timeout=timeout)
    if exit_ != 0:
        return None, f"motion rt-capture exited {exit_} ({dur:.0f}s)"
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "capture.log").write_text(out + err)
    log(f"[rt-noise]   motion captured in {dur:.0f}s → {run_dir}")
    return err, None


def parse_motion_log(stderr):
    """Capture lines → {channel: [(frame, hit, mean0, center0, center3), ...]}."""
    series = {}
    for line in stderr.splitlines():
        m = MOTION_CAPTURE_RE.search(line)
        if not m:
            continue
        ch, frame, hit = m.group(1), int(m.group(2)), float(m.group(3))
        mean0 = float(m.group(4).split(",")[0])
        centers = m.group(5).split(",")
        center0 = float(centers[0])
        # moments' 4th center component is the per-texel history length —
        # D-64's own measurable: pre-fix it oscillated 16→1.3 under orbit,
        # post-fix it converges 68+. center[3] is absent on channels that
        # don't carry it; NaN sorts out of the metric.
        center3 = float(centers[3]) if len(centers) > 3 else float("nan")
        series.setdefault(ch, []).append((frame, hit, mean0, center0, center3))
    for ch in series:
        series[ch].sort()
    return series


def measure_motion(runs_series):
    """The leg's statistics across runs.

    sv_hold: MEDIAN across captures, not max — a genuine edge crossing
    trips the gate occasionally under motion and SHOULD (BUG-tr5o: the
    residual is real change); the re-trip signature is the median staying
    high, not a spike. max is reported, never gated.
    moments history: the LATE-window median (last 3 captures per run) —
    early captures are still converging even when healthy (~20), so a
    whole-window min gates the ramp, not the disease. D-64's measurable:
    pre-fix ~10 with salt speckle at convergence, post-fix 68+."""
    sv = [pt[2] for r in runs_series for pt in r.get("sv_hold", [])]
    mom_center0 = [pt[3] for r in runs_series for pt in r.get("moments", [])]
    # History RATIO per run (min/median of the moments history component
    # across the window) — self-normalizing, so a scene whose healthy
    # convergence cycles low (the car fixture's 10-25) passes while the
    # boil signature (oscillation down to ~1.3, ratio ~0.08) trips. An
    # absolute floor false-fails the low-cycling healthy scene.
    ratios = []
    for r in runs_series:
        h = [pt[4] for pt in r.get("moments", [])]
        if h:
            ratios.append(min(h) / max(statistics.median(h), 1e-9))
    refl_hit_min = min((min(pt[1] for pt in r.get("refl_raw", [(0, 1.0, 0, 0, 0)]))
                        for r in runs_series), default=1.0)
    return {
        "sv_hold_median": statistics.median(sv) if sv else 0.0,
        "sv_hold_max": max(sv) if sv else 0.0,
        "moments_center_min": min(mom_center0) if mom_center0 else 0.0,
        "moments_center_median": statistics.median(mom_center0) if mom_center0 else 0.0,
        "history_ratio_min": min(ratios) if ratios else 0.0,
        "refl_hit_min": refl_hit_min,
    }


def motion_leg(repo, binary, project, baseline, record, baseline_path, out_dir, repeats):
    """The motion leg. Returns an exit code (0 green/skip, 1 red, 2 unknown)."""
    runs_series = []
    attempts = 0
    max_attempts = repeats + 2
    LOCK_PATH.touch()
    with open(LOCK_PATH, "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        while len(runs_series) < repeats and attempts < max_attempts:
            attempts += 1
            log(f"[rt-noise] motion run {len(runs_series) + 1}/{repeats} (attempt {attempts})")
            err, why = motion_capture_once(
                binary, project, out_dir / f"motion{attempts}", repo,
                timeout=max(900, MOTION_FRAMES * 6))
            if why:
                log(f"[rt-noise]   DISCARD: {why}")
                continue
            series = parse_motion_log(err)
            if "refl_raw" not in series or "sv_hold" not in series or "moments" not in series:
                missing = {"refl_raw", "sv_hold", "moments"} - set(series)
                log(f"[rt-noise]   DISCARD: capture log lacks channel(s): {sorted(missing)}")
                continue
            # Empty-accel rejection (BUG-oqta class): a run whose refl_raw
            # never hit anything is a dead channel, not a calm one.
            hit_min = min(pt[1] for pt in series["refl_raw"])
            if hit_min <= 0.0:
                log(f"[rt-noise]   DISCARD: refl_raw hit == 0 across the window "
                    f"(empty-accel class — the leg cannot see anything)")
                continue
            runs_series.append(series)

    if len(runs_series) < repeats:
        log(f"[FAIL] only {len(runs_series)}/{repeats} clean motion run(s) "
            f"in {attempts} attempts")
        return 2

    m = measure_motion(runs_series)
    log(f"\nmotion leg (median of {len(runs_series)} run(s)): "
        f"sv_hold median {m['sv_hold_median']:.4f} max {m['sv_hold_max']:.4f} · "
        f"moments center-R min {m['moments_center_min']:.3f} "
        f"median {m['moments_center_median']:.3f} · "
        f"moments history ratio min {m['history_ratio_min']:.2f} · "
        f"refl_raw hit min {m['refl_hit_min']:.3f}")

    if record:
        doc = baseline if baseline else {}
        doc["motion"] = {
            "validated": True,
            "sv_hold_median": round(m["sv_hold_median"] * 3.0 + 0.002, 4),
            "moments_center_min": round(m["moments_center_min"] * 0.7, 4),
            "history_ratio_min": round(min(m["history_ratio_min"] * 0.5, 0.3), 4),
            "measured": {k: round(v, 4) for k, v in m.items()},
            "note": ("Motion-leg ceilings = measured x headroom (sv_hold median "
                     "x3+0.002, moments floors x0.7, history ratio x0.5 capped 0.3). "
                     "Regenerate with "
                     "scripts/rt_noise_gate.py --motion --record after a legitimate "
                     "RT change and commit this file. RED-validated against "
                     "boil-era gate behavior (D-64/tr5o reverted) — see BUG-sz0u."),
        }
        baseline_path.write_text(json.dumps(doc, indent=2) + "\n")
        log(f"[rt-noise] wrote motion ceilings to {baseline_path}")
        return 0

    motion_b = (baseline or {}).get("motion")
    if not motion_b or not motion_b.get("validated"):
        log("[SKIP] no validated motion ceilings in the baseline — run "
            "`scripts/rt_noise_gate.py --motion --record` after the red-side "
            "validation (BUG-sz0u) and commit the JSON.")
        return 0

    failures = []
    if m["sv_hold_median"] > motion_b["sv_hold_median"]:
        failures.append(f"sv_hold median {m['sv_hold_median']:.4f} > ceiling "
                        f"{motion_b['sv_hold_median']:.4f} — the sv gate is "
                        f"re-tripping under motion (BUG-tr5o class)")
    if m["moments_center_min"] < motion_b["moments_center_min"]:
        failures.append(f"moments center-R min {m['moments_center_min']:.3f} < floor "
                        f"{motion_b['moments_center_min']:.3f} — the GI estimate "
                        f"collapses under camera motion (D-64 boil class)")
    if m["history_ratio_min"] < motion_b["history_ratio_min"]:
        failures.append(f"moments history ratio {m['history_ratio_min']:.2f} < floor "
                        f"{motion_b['history_ratio_min']:.2f} — accumulation history "
                        f"collapses under camera motion (D-64 boil: ratio ~0.08 pre-fix "
                        f"vs ~1.0 healthy)")
    if failures:
        log("\nRT NOISE GATE (motion): RED")
        for f in failures:
            log(f"  {f}")
        return 1
    log("\nRT NOISE GATE (motion): green")
    return 0



def write_baseline(path, agg, repo, project, frames, repeats):
    commit = run_cmd(["git", "rev-parse", "--short=12", "HEAD"], cwd=repo, timeout=30)[1].strip()
    dirty = run_cmd(["git", "status", "--porcelain"], cwd=repo, timeout=60)[1].strip() != ""
    channels = {}
    for ch, a in agg.items():
        mm, mf = HEADROOM["mean"]
        pm, pf = HEADROOM["p999"]
        channels[ch] = {
            "mean": round(max(a["mean"] * mm, a["mean"] + mf), 4),
            "p999": round(max(a["p999"] * pm, a["p999"] + pf), 3),
            "min_signal_level": round(a["level"] * SIGNAL_FLOOR_FRACTION, 4),
            "measured": {k: round(a[k], 4) for k in
                         ("mean", "mean_min", "mean_max", "p999", "p999_min",
                          "p999_max", "max", "level")},
        }
    doc = {
        "schema": 1,
        "ceilings_validated": True,
        "note": ("Ceilings = measured median x headroom (mean x2.0 or +0.05, "
                 "p99.9 x1.8 or +1.0). Regenerate with "
                 "scripts/rt_noise_gate.py --record after a legitimate RT "
                 "improvement and commit this file."),
        "provenance": {
            "recorded": datetime.now(timezone.utc).strftime("%Y-%m-%d"),
            "commit": commit + ("-dirty" if dirty else ""),
            "project": Path(project).name,
            "frames": frames,
            "repeats": repeats,
            "statistic": "per-channel median across clean repeats",
        },
        "channels": channels,
    }
    path.write_text(json.dumps(doc, indent=2) + "\n")
    log(f"\n[rt-noise] wrote {path}")


# ── main ────────────────────────────────────────────────────────────────


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", default=Path.cwd(), help="repo path (default: cwd)")
    ap.add_argument("--project", default=None, help="override the fixture project")
    ap.add_argument("--binary", default=None, help="use this manifold binary, skip the build")
    ap.add_argument("--frames", type=int, default=300, help="rt-capture frame count (default 300)")
    ap.add_argument("--repeats", type=int, default=3, help="clean runs to median (default 3)")
    ap.add_argument("--max-attempts", type=int, default=None,
                    help="cap on runs including discards (default 2x repeats + 3)")
    ap.add_argument("--baseline", default=None, help="ceilings JSON (default scripts/rt_noise_baseline.json)")
    ap.add_argument("--out-dir", default=None, help="where to keep captures (default target/rt-noise-gate)")
    ap.add_argument("--record", action="store_true", help="write the ceilings from this run instead of gating")
    ap.add_argument("--require-fixture", action="store_true",
                    help="a missing fixture is a failure, not a skip (nightly uses this)")
    ap.add_argument("--json", action="store_true", help="also emit the aggregate as JSON")
    ap.add_argument("--skip-furnace", action="store_true",
                    help="stability leg only — do not run the furnace oracle "
                         "(recording, or isolating a stability issue)")
    ap.add_argument("--motion", action="store_true",
                    help="run the MOTION leg (BUG-sz0u) instead of the static "
                         "stability leg: camera-orbit ramp, sv_hold/moments/"
                         "refl_raw log metrics, no PNG math, no furnace leg")
    args = ap.parse_args()

    repo = Path(args.repo).resolve()
    baseline_path = Path(args.baseline) if args.baseline else repo / BASELINE
    out_dir = Path(args.out_dir) if args.out_dir else repo / "target/rt-noise-gate"

    project = resolve_fixture(repo, args.project)
    if project is None:
        msg = (f"RT noise fixture not found. Looked at --project, "
               f"$MANIFOLD_RT_NOISE_PROJECT, {repo / FIXTURE_REL}, "
               f"{MAIN_CHECKOUT / FIXTURE_REL}. .manifold fixtures are "
               f"gitignored; put the project at {FIXTURE_REL} in the main "
               f"checkout and agent-worktree.py copies it into every slot.")
        if args.require_fixture:
            log(f"[FAIL] {msg}")
            return 2
        log(f"[SKIP] {msg}")
        return 0

    baseline = None
    if baseline_path.exists() and (not args.record or args.motion):
        # Load even when recording the motion leg: --motion --record must
        # PRESERVE the static ceilings, not wipe them with a motion-only doc.
        baseline = json.loads(baseline_path.read_text())
    if not args.record:
        if not baseline_path.exists():
            log(f"[SKIP] no ceilings at {baseline_path} — run --record first")
            return 0
        if not baseline.get("ceilings_validated"):
            log(f"[SKIP] {baseline_path} is marked unvalidated: "
                f"{baseline.get('note', '')} — run --record against a working "
                f"RT path, commit the JSON, and this gate starts guarding.")
            return 0

    # ED7 correctness leg, FIRST: the furnace oracle is a seconds-cost debug
    # test, so a correctness regression is caught before the minutes-cost
    # release build + 300-frame stability capture ever starts. Green requires
    # BOTH legs — stability alone cannot certify (the frozen-seed trap,
    # BUG-ipad). `--record` also gates on it: a stability ceiling recorded
    # against a broken RT path would be garbage.
    if not args.skip_furnace and not args.motion:
        furnace_ok, furnace_out = run_furnace_oracle(repo)
        if not furnace_ok:
            log("[rt-noise] furnace oracle RED — correctness leg failed; "
                "stability is not the verdict. Tail:")
            for line in furnace_out.rstrip().splitlines()[-8:]:
                log(f"    {line}")
            log("\nRT NOISE GATE: RED (furnace oracle)")
            return 1

    binary = Path(args.binary).resolve() if args.binary else build_binary(repo)
    if binary is None:
        return 2
    if not binary.exists():
        log(f"[FAIL] no binary at {binary}")
        return 2

    log(f"[rt-noise] project={project}")
    log(f"[rt-noise] frames={args.frames} repeats={args.repeats}")

    if out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True)

    if args.motion:
        return motion_leg(repo, binary, project, baseline, args.record,
                          baseline_path, out_dir, args.repeats)

    LOCK_PATH.touch()
    with open(LOCK_PATH, "w") as lock:
        # rt_capture.rs dumps to a hardcoded /tmp path; concurrent runs would
        # interleave PNGs and corrupt both metrics.
        fcntl.flock(lock, fcntl.LOCK_EX)

        runs = []
        attempts = 0
        # Headroom for a bad streak, not just one bad run. `repeats + 2` ran out
        # of attempts mid-measurement while captures were being discarded, and
        # exit 2 because the budget was thin reads the same as exit 2 because the
        # tree is broken.
        max_attempts = args.max_attempts or (args.repeats * 2 + 3)
        discarded = []
        while len(runs) < args.repeats and attempts < max_attempts:
            attempts += 1
            log(f"[rt-noise] run {len(runs) + 1}/{args.repeats} (attempt {attempts})")
            run_dir, err = capture_once(binary, project, args.frames,
                                        out_dir / f"attempt{attempts}", repo,
                                        timeout=max(900, args.frames * 4))
            if run_dir is None:
                log(f"[rt-noise]   DISCARD: {err}")
                discarded.append(err)
                continue
            stats, window_start = measure(run_dir)
            if not stats:
                log("[rt-noise]   DISCARD: no consecutive frame pairs in the capture")
                discarded.append("no consecutive pairs")
                continue
            pairs = min(v["pairs"] for v in stats.values())
            if pairs < MIN_PAIRS:
                log(f"[FAIL] only {pairs} consecutive pair(s); the metric needs "
                    f"{MIN_PAIRS}. rt_capture's paused-phase consecutive run "
                    f"changed — fix the harness, do not lower this.")
                return 2
            bad, why = contaminated(err, window_start)
            if bad:
                log(f"[rt-noise]   DISCARD: {why}")
                discarded.append(why)
                continue
            dead = dead_channels(stats)
            if dead:
                why = f"channel(s) produced nothing at all: {', '.join(dead)}"
                log(f"[rt-noise]   DISCARD: {why} (BUG-mw0x)")
                discarded.append(why)
                continue
            runs.append(stats)

        if len(runs) < args.repeats:
            log(f"[FAIL] only {len(runs)}/{args.repeats} clean run(s) in "
                f"{attempts} attempts: {'; '.join(discarded)}")
            return 2
        # Always visible, not just on failure: the discard rate IS the harness
        # health, and it is the number that says whether this gate is trustable.
        log(f"[rt-noise] {len(runs)} clean run(s) from {attempts} attempt(s), "
            f"{len(discarded)} discarded")
        for why in discarded:
            log(f"[rt-noise]   discarded: {why}")

    agg = median_across(runs)
    if args.record:
        print_table([(ch, agg[ch], None, "recorded") for ch in sorted(agg)])
        print_spread(agg)
        write_baseline(baseline_path, agg, repo, project, args.frames, args.repeats)
        return 0

    failures, rows = compare(agg, baseline)
    print_table(rows)
    print_spread(agg)
    if args.json:
        log("\n" + json.dumps(agg, indent=2))
    log(f"\nceilings from {baseline_path.name} "
        f"(recorded {baseline['provenance']['recorded']} @ "
        f"{baseline['provenance']['commit']})")
    if failures:
        log("\nRT NOISE GATE: RED")
        for ch, why in failures:
            log(f"  {ch}: {why}")
        log("A regression here is visible on stage as a static shot that "
            "crawls. Re-record only if the change is a deliberate, accepted "
            "quality trade.")
        return 1
    log("\nRT NOISE GATE: green")
    return 0


if __name__ == "__main__":
    sys.exit(main())
