#!/usr/bin/env python3
"""RT toggle matrix harness over the headless `rt-capture` subcommand.

One `manifold rt-capture` process per matrix cell. Each cell flips one RT
toggle live (`--live-flip`) or moves the sun (a lighting move), and this
harness parses the per-frame stats and live-flip verdicts rt-capture prints to
stderr, then flags anomalous behavior per cell.

THE BUG CLASS THIS CATCHES
BUG-18l: a live toggle that is INERT — the flip is issued, the value changes,
but nothing in the render does. rt-capture already prints per-channel
before/after verdicts (`stats changed` / `APPEARED` / `VANISHED`) and a
`LIVE-FLIP EFFECTIVE` / `LIVE-FLIP INERT` line; this harness runs every toggle
through that path and turns the verdicts plus the raw per-frame stats into one
PASS/WARN/FAIL line per cell. It also covers the raster-fallback cells: when
`rt_enabled` flips off, the RT channels must vanish or collapse while the
composite survives on the raster path and changes.

UNIVERSAL CHECKS (every cell)
  INERT   flip run with no changed/APPEARED/VANISHED verdict (the BUG-18l class).
  NaN/INF any non-finite hit/luma/sd value in a per-frame stats line.
  BLACK   composite hit-fraction == 0 or luma == 0 at the final capture.
  MISSING zero PNGs written, or the run exited non-zero, or a param failed to
          resolve (`Param '...' not found`).

DIRECTION CHECKS (flip cells) run only when before/after composite stats both
exist. The flip boundary is frame 60 — rt_capture.rs plays `rotation_frames =
60` in `--paused` mode before sending the flip. A missed direction is a WARN
(scene-dependent) except on `rt_shadows` and `rt_denoise_feed`, where the
direction is load-bearing and a miss is a FAIL.

Fixture default is the MAIN checkout's (`fixtures/` is gitignored and not
copied into every worktree slot). Override with `--project`.
"""

import argparse
import math
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

MAIN_CHECKOUT = Path("/Users/peterkiemann/MANIFOLD - Rust")
FIXTURE_REL = Path("tests/fixtures/rt/RtNoiseTesting.manifold")

# ── cell table ─────────────────────────────────────────────────────────
# Flip cells: --paused, so rt-capture plays 60 frames, flips the toggle, then
# plays 120 more (flip phase) + 60 paused. Lighting cells: continuous, RT fully
# on, no flip — --animate/--set-at only run inside the phase-1 play loop, so
# they need a non-paused run for the frame numbers to land (--set-at 60 fires
# only when rotation_frames == total_frames).
FLIP_COMMON = ["--paused", "--frames", "120", "--width", "1280", "--height", "720"]
LIGHT_COMMON = ["--frames", "120", "--width", "1280", "--height", "720"]

CELLS = [
    # (name, kind, extra rt-capture args after the project position)
    ("rt_enabled",        "flip",     FLIP_COMMON + ["--live-flip", "rt_enabled"]),
    ("rt_shadows",        "flip",     FLIP_COMMON + ["--live-flip", "rt_shadows"]),
    ("rt_ao",             "flip",     FLIP_COMMON + ["--live-flip", "rt_ao"]),
    ("rt_gi",             "flip",     FLIP_COMMON + ["--live-flip", "rt_gi"]),
    ("rt_reflections",    "flip",     FLIP_COMMON + ["--live-flip", "rt_reflections"]),
    ("rt_denoise_feed",   "flip",     FLIP_COMMON + ["--live-flip", "rt_denoise_feed"]),
    ("temporal_upscale",  "flip",     FLIP_COMMON + ["--live-flip", "temporal_upscale"]),
    ("sun-x-ramp",        "lighting", LIGHT_COMMON + ["--animate", "pos_x", "0.05",
                                                      "--capture-every", "20",
                                                      "--capture-from", "40",
                                                      "--frame-clock"]),
    ("sun-intensity-snap", "lighting", LIGHT_COMMON + ["--set-at", "60", "intensity=8.0"]),
]

# Direction each flip cell expects for the composite, and whether a miss is a
# FAIL (load-bearing) or a WARN (scene-dependent). The universal INERT check is
# separate and always a FAIL. rt_enabled is handled by its own branch above
# (RT channels must vanish/collapse, composite must change).
FLIP_DIRECTION = {
    "rt_shadows":       ("luma_rise",    "FAIL"),
    "rt_ao":            ("luma_rise",    "WARN"),
    "rt_gi":            ("luma_shift",   "WARN"),
    "rt_reflections":   ("any_change",   "WARN"),
    "rt_denoise_feed":  ("sd_rise",      "FAIL"),
    "temporal_upscale": ("any_change",   "WARN"),
}

# ── thresholds ─────────────────────────────────────────────────────────
# rt_capture.rs itself calls a before/after "changed" at |delta| > 0.01 on hit
# or luma. The direction checks reuse that scale but split per metric because
# sd (luma stddev, linear domain) is an order of magnitude smaller than luma.
CHANGE_HIT = 0.01
CHANGE_LUMA = 0.01
RISE_LUMA = 1e-3          # "luma rises" — real brightening, not the 0.01 noise floor
SHIFT_LUMA = 1e-3         # "luma shifts" — any direction, just not flat
RISE_SD_REL = 0.05        # "sd rises" — 5% relative (sd is small and unit-dependent)
RISE_SD_ABS = 1e-4
RAMP_SPREAD = 1e-3        # sun-x-ramp: composite must move across captures by this much
SNAP_JUMP_REL = 0.5       # sun-intensity-snap: luma must jump >=50% relative after frame 60

# rt_capture.rs prints this stdout marker the instant the flip is sent.
FLIP_SPLIT_RE = re.compile(r"=== LIVE FLIP")


def split_at_flip(text):
    """Split a log at the flip point into (pre, post) halves.

    The split must be on LOG ORDER, not frame number. rt-capture's flip phase
    runs frames 60..180 while the paused phase reuses frames 60..120, so frame
    numbers are non-monotonic — a capture at f=150 appears in the log BEFORE
    the final settled f=119. Splitting on the marker line gives the settled
    post-flip state as the last composite line instead of a mid-flip transient.
    """
    m = FLIP_SPLIT_RE.search(text)
    if not m:
        return text, ""
    return text[:m.start()], text[m.start():]


def composite_series(text):
    """Composite (frame, hit, luma, sd) rows in log order from a text blob."""
    out = []
    for line in text.splitlines():
        m = STATS_RE.search(line)
        if m and m.group("label") == "composite":
            try:
                out.append((int(m.group("frame")),
                            _num(m.group("hit")), _num(m.group("luma")), _num(m.group("sd"))))
            except ValueError:
                continue
    return out

# rt_capture.rs stderr formats (rt_capture.rs: process_capture + live-flip verdict):
#   [rt-capture] <label> f=NNNN dim=WxH hit=H luma=L sd=S mean=[...] center=[...] path
#   [rt-capture] <label> stats changed: hit X → Y, luma A → B
#   [rt-capture] <label> APPEARED after flip: hit H, luma L
#   [rt-capture] <label> VANISHED after flip (was hit H, luma L)
STATS_RE = re.compile(
    r"\[rt-capture\] (?P<label>\S+) f=(?P<frame>\d+) dim=\S+ "
    r"hit=(?P<hit>\S+) luma=(?P<luma>\S+) sd=(?P<sd>\S+)"
)
CHANGED_RE = re.compile(r"\[rt-capture\] (?P<label>\S+) stats changed:")
APPEARED_RE = re.compile(r"\[rt-capture\] (?P<label>\S+) APPEARED after flip:")
VANISHED_RE = re.compile(r"\[rt-capture\] (?P<label>\S+) VANISHED after flip")
PARAM_NOT_FOUND_RE = re.compile(r"\[rt-capture\] Param '([^']+)' not found")


def log(msg):
    print(msg, flush=True)


def _num(tok):
    """Parse a Rust `{:.6}` float token, which prints NaN/inf literally."""
    t = tok.lower()
    if "nan" in t:
        return float("nan")
    if "inf" in t:
        return float("-inf") if t.startswith("-") else float("inf")
    return float(t)


def parse_log(text):
    stats = {}          # label -> list[(frame, hit, luma, sd)]
    verdicts = []       # list[(kind, label)]  kind in {changed, appeared, vanished}
    for line in text.splitlines():
        m = STATS_RE.search(line)
        if m:
            try:
                hit = _num(m.group("hit"))
                luma = _num(m.group("luma"))
                sd = _num(m.group("sd"))
            except ValueError:
                continue
            stats.setdefault(m.group("label"), []).append(
                (int(m.group("frame")), hit, luma, sd)
            )
            continue
        for rex, kind in ((CHANGED_RE, "changed"),
                          (APPEARED_RE, "appeared"),
                          (VANISHED_RE, "vanished")):
            m = rex.search(line)
            if m:
                verdicts.append((kind, m.group("label")))
                break
    return stats, verdicts


def resolve_fixture(repo, explicit):
    for cand in (explicit, repo / FIXTURE_REL, MAIN_CHECKOUT / FIXTURE_REL):
        if cand and Path(cand).exists():
            return Path(cand)
    return None


def run_cmd(cmd, cwd, env, timeout):
    start = time.time()
    try:
        # stderr=STDOUT merges the two streams into one pipe in write order.
        # rt-capture prints stats/verdicts to stderr and the `=== LIVE FLIP ===`
        # marker to stdout; capturing them separately would put all stdout
        # before all stderr and destroy the temporal ordering the flip split
        # depends on.
        r = subprocess.run(cmd, cwd=str(cwd), stdout=subprocess.PIPE,
                           stderr=subprocess.STDOUT, text=True, env=env,
                           timeout=timeout)
    except subprocess.TimeoutExpired:
        return -1, f"TIMEOUT after {timeout}s: {' '.join(map(str, cmd))}", time.time() - start
    except (FileNotFoundError, PermissionError) as e:
        return -1, f"cannot run {' '.join(map(str, cmd))}: {e}", time.time() - start
    return r.returncode, r.stdout or "", time.time() - start


def check_cell(name, kind, parsed, png_count, rc, log_text):
    stats, verdicts = parsed
    reasons = []

    def fail(msg):
        reasons.append(f"FAIL:{msg}")

    def warn(msg):
        reasons.append(f"WARN:{msg}")

    # Param resolution failure surfaces as a clean exit-1 before any capture.
    m = PARAM_NOT_FOUND_RE.search(log_text)
    if m:
        fail(f"param '{m.group(1)}' not found (no layer mutation took effect)")
    if rc != 0:
        fail(f"rt-capture exited {rc}")

    # MISSING — zero PNGs written. Counted on the per-cell capture dir.
    if png_count == 0:
        fail("0 PNGs written")

    # NaN/INF — any non-finite hit/luma/sd anywhere.
    nonfinite = sorted(
        f"{lab}@{fr}" for lab, rows in stats.items()
        for (fr, h, l, s) in rows
        if not (math.isfinite(h) and math.isfinite(l) and math.isfinite(s))
    )
    if nonfinite:
        fail(f"non-finite stats: {', '.join(nonfinite)}")

    # Composite in LOG ORDER (== temporal order). The final settled capture is
    # the last composite line, which is frame 119 in --paused mode.
    comp = composite_series(log_text)
    if not comp:
        fail("no composite stats parsed")
        comp_last = None
    else:
        comp_last = comp[-1]

    # BLACK — the composite is fully black at the final capture. For the
    # rt_enabled cell the final capture is the raster fallback; it must be
    # non-black too.
    if comp_last is not None:
        _, h, l, _s = comp_last
        if h == 0.0 or l == 0.0:
            fail(f"BLACK composite at f={comp_last[0]} hit={h:.6f} luma={l:.6f}")

    if kind == "flip":
        changed_any = len(verdicts) > 0
        if not changed_any:
            fail("INERT live flip — no channel changed/APPEARED/VANISHED (BUG-18l class)")

        pre, post = split_at_flip(log_text)
        b = composite_series(pre)[-1] if composite_series(pre) else None
        a = composite_series(post)[-1] if composite_series(post) else None

        # rt_enabled: RT channels must vanish or collapse, composite must change.
        if name == "rt_enabled":
            vanished = [lab for (k, lab) in verdicts if k == "vanished" and lab != "composite"]
            changed_noncomp = [lab for (k, lab) in verdicts if k == "changed" and lab != "composite"]
            if not vanished and not changed_noncomp:
                warn("no RT channel vanished or changed — raster fallback not exercised")
            if b and a and abs(a[2] - b[2]) < CHANGE_LUMA and abs(a[1] - b[1]) < CHANGE_HIT:
                warn(f"composite did not change across rt_enabled flip (luma {b[2]:.6f}→{a[2]:.6f})")

        # Direction check for the remaining flip cells.
        if name in FLIP_DIRECTION and name != "rt_enabled":
            direction, severity = FLIP_DIRECTION[name]
            if b is not None and a is not None:
                dh = a[1] - b[1]   # hit delta
                dl = a[2] - b[2]   # luma delta
                ds = a[3] - b[3]   # sd delta
                ok = False
                if direction == "luma_rise":
                    ok = dl > RISE_LUMA
                elif direction == "luma_shift":
                    ok = abs(dl) > SHIFT_LUMA or abs(dh) > CHANGE_HIT
                elif direction == "sd_rise":
                    ok = (ds > RISE_SD_ABS and a[3] > b[3] * (1 + RISE_SD_REL))
                elif direction == "any_change":
                    ok = abs(dl) > CHANGE_LUMA or abs(dh) > CHANGE_HIT
                if not ok:
                    msg = (f"composite direction '{direction}' not met "
                           f"(hit {b[1]:.6f}→{a[1]:.6f}, luma {b[2]:.6f}→{a[2]:.6f}, "
                           f"sd {b[3]:.6f}→{a[3]:.6f})")
                    (fail if severity == "FAIL" else warn)(msg)
            else:
                (fail if severity == "FAIL" else warn)(
                    "no before/after composite stats to run the direction check")

    elif kind == "lighting":
        # Lighting cells are continuous (no flip): frame numbers are monotonic,
        # so frame-based before/after is safe here.
        if name == "sun-x-ramp":
            lumas = [r[2] for r in comp]
            hits = [r[1] for r in comp]
            spread = (max(lumas) - min(lumas)) if lumas else 0.0
            hspread = (max(hits) - min(hits)) if hits else 0.0
            if spread < RAMP_SPREAD and hspread < CHANGE_HIT:
                fail(f"composite inert across sun-x ramp "
                     f"(luma spread {spread:.6f}, hit spread {hspread:.6f})")
        elif name == "sun-intensity-snap":
            b = max((r for r in comp if r[0] <= 60), default=None)
            a = max((r for r in comp if r[0] > 60), default=None)
            if b is not None and a is not None:
                need = max(SNAP_JUMP_REL * b[2], RISE_LUMA)
                if a[2] <= b[2] + need:
                    fail(f"composite luma did not jump after --set-at 60 "
                         f"(f{b[0]} {b[2]:.6f} → f{a[0]} {a[2]:.6f})")
            else:
                fail("no before/after composite stats across the intensity snap")

    return reasons


def run_cell(name, kind, extra_args, binary, project, out_root, timeout):
    cell_dir = out_root / name
    if cell_dir.exists():
        shutil.rmtree(cell_dir)
    cell_dir.mkdir(parents=True)

    # rt-capture resolves the project as the FIRST non-`--` arg (positional
    # scan), so the project path must precede every flag that carries a bare
    # value (`--frames 120`, `--live-flip <param>`, …) or the value wins.
    cmd = [str(binary), "rt-capture", str(project), *extra_args]
    env = dict(os.environ)
    env["MANIFOLD_RT_CAPTURE_DIR"] = str(cell_dir)

    rc, text, dur = run_cmd(cmd, cwd=out_root, env=env, timeout=timeout)
    (cell_dir / "run.log").write_text(text)

    pngs = sorted(cell_dir.glob("*.png"))
    parsed = parse_log(text)
    reasons = check_cell(name, kind, parsed, len(pngs), rc, text)

    if any(r.startswith("FAIL:") for r in reasons):
        verdict = "FAIL"
    elif any(r.startswith("WARN:") for r in reasons):
        verdict = "WARN"
    else:
        verdict = "PASS"
    detail = "; ".join(r[5:] for r in reasons) if reasons else "ok"
    log(f"CELL {name}: {verdict} — {detail}")
    return verdict, reasons, cell_dir, pngs, dur


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--binary", default=str(Path(__file__).resolve().parent.parent / "target/release/manifold"),
                    help="path to the perf-soak manifold binary")
    ap.add_argument("--project", default=None, help="override the .manifold fixture")
    ap.add_argument("--outdir", default="/tmp/rt_matrix", help="per-cell output root (default /tmp/rt_matrix)")
    ap.add_argument("--only", action="append", default=[], metavar="CELL",
                    help="run only this cell (repeatable); default all")
    ap.add_argument("--timeout", type=int, default=None,
                    help="per-cell timeout in seconds (default: none)")
    ap.add_argument("--list", action="store_true", help="list cells and exit")
    args = ap.parse_args()

    repo = Path(__file__).resolve().parent.parent
    if args.list:
        for name, kind, extra in CELLS:
            print(f"{name:22s} {kind:8s} {' '.join(extra)}")
        return 0

    binary = Path(args.binary)
    if not binary.exists():
        log(f"[rt-matrix] binary not found: {binary} — build with "
            f"`cargo build --release --features perf-soak --bin manifold` first")
        return 2
    project = resolve_fixture(repo, args.project)
    if project is None:
        log("[rt-matrix] fixture not found (set --project or place it at "
            f"{MAIN_CHECKOUT / FIXTURE_REL})")
        return 2

    out_root = Path(args.outdir)
    out_root.mkdir(parents=True, exist_ok=True)

    selected = [c for c in CELLS if not args.only or c[0] in args.only]
    if args.only:
        missing = [o for o in args.only if o not in [c[0] for c in CELLS]]
        if missing:
            log(f"[rt-matrix] unknown cell(s): {', '.join(missing)}")
            return 2

    log(f"[rt-matrix] binary={binary}")
    log(f"[rt-matrix] project={project}")
    log(f"[rt-matrix] cells={[c[0] for c in selected]}")

    results = {}
    for name, kind, extra in selected:
        results[name] = run_cell(name, kind, extra, binary, project, out_root, args.timeout)

    p = sum(1 for v, *_ in results.values() if v == "PASS")
    w = sum(1 for v, *_ in results.values() if v == "WARN")
    f = sum(1 for v, *_ in results.values() if v == "FAIL")
    log(f"TOTALS: {len(results)} cells, {p} PASS, {w} WARN, {f} FAIL")
    return 1 if f else 0


if __name__ == "__main__":
    sys.exit(main())
