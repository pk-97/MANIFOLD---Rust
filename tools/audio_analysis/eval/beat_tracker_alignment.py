"""D14 grid absolute alignment for the beat tracker stage (P2).

eval/click_track.py's D14 fixtures (sparse, non-periodic tone bursts, one
every ~1.7s) were built to measure decode-stage skew and the OLD madmom
onset CNN's attack-vs-truth bias — an onset detector that scores arbitrary
transients one at a time. Beat This is a different kind of model: it tracks
*periodic musical beats* from a whole-clip mel-spectrogram, and correctly
reports ZERO beats on non-periodic tone bursts (measured 2026-07-17: 0/12
detections on all three eval/data/click_track_fixtures formats) — that is
not a bug, and the sparse click fixture cannot exercise this stage at all.

The correct D14 fixture shape for a beat tracker is a PERIODIC click track at
a real tempo, so Beat This actually tracks it as a beat pattern. Reusing
click_track.py's exact burst-synthesis + wav/mp3/AAC export path (same
sample-accurate placement machinery, same lossy encode settings) with a
broadband percussive envelope instead of a pure tone (measured 2026-07-17: a
pure 3kHz Hann tone, like the sparse fixture uses, ALSO produces zero
detections — Beat This's mel-spectrogram frontend needs broadband transient
energy resembling a real drum hit, not a narrow tone; a short exponentially-
decaying noise burst is used here instead).

What is measured, per format (wav/mp3/AAC): the max absolute difference, in
ms, between Beat This's detected beat times on that format vs on wav (the
reference — wav decode has ~0 skew per P1's own decode-stage measurement).
This is the literal analogue of P1's decode/detector-stage split applied to
the beat tracker instead of the onset CNN: does format change the grid Beat
This builds. Absolute alignment to the KNOWN click positions is reported too
(diagnostic) — edge beats (first ~1-2, where the model has no prior context
to lock onto tempo) are excluded from that number by construction, a known,
universal beat-tracker property, not specific to this port.

P3 addendum (BUG-229 follow-up): P2 measured RAW per-beat alignment against
this periodic fixture at 128 BPM — median 14.4 ms / max 26.25 ms, a clean
sawtooth from Beat This's 50fps (20ms) frame quantization, worse than D14's
5ms target. The recorded hypothesis: quantization noise should average out
once you build the FITTED regular grid actually consumed downstream (median
inter-beat-interval -> one BPM number + one anchor phase -> a regular grid
over the whole track), rather than scoring each raw quantized beat position
individually. `fit_regular_grid_from_beats()` / `measure_fitted_grid_alignment()`
below measure exactly that, reusing `manifold_audio.bpm._build_regular_beat_grid`
(the same primitive `_build_beat_grid()` calls in its own "synthetic" mode) —
no new grid-construction logic is invented here.

Honest caveat found while wiring this up: `_build_beat_grid()`'s "tracker"
mode (i.e. what actually ships when Beat This returns >=2 beats, which is
every real-world case) does NOT rebuild a regular grid from the raw tracker
beat times — it keeps them as-is (only extending coverage at the two edges
via median-IBI extrapolation, see `_extend_beat_times_coverage`). The fully
regularized grid built from median IBI + a fitted anchor is what
`_build_beat_grid()` produces only in its "synthetic" (no-tracker) branch.
So this measurement answers "if the raw tracker beats were regularized the
way the synthetic path already does, would alignment clear 5ms" — a
measurement of the fitted-grid hypothesis using the exact existing
primitive, not a claim that today's production code already takes this path
for tracker output. That gap (tracker mode not regularizing) is exactly
BUG-229's still-open verification debt for the P5 correction seam to decide.
"""

from __future__ import annotations

import json
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Dict, List, Optional

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from eval.click_track import encode_lossy, write_wav  # noqa: E402
from manifold_audio.beat_tracking import bpm_from_beat_times, estimate_beats_this  # noqa: E402
from manifold_audio.bpm import _build_regular_beat_grid, _normalize_bpm  # noqa: E402

SAMPLE_RATE = 44100
BPM = 128.0  # electronic-domain tempo (fixtures.toml's default target genre)
N_BEATS = 40
START_SEC = 0.75
CLICK_DUR_SEC = 0.05
CLICK_DECAY_SEC = 0.008
EDGE_BEATS_EXCLUDED = 2  # model needs ~1 bar of context to lock onto tempo


def _percussive_click(sr: int) -> np.ndarray:
    n = int(round(CLICK_DUR_SEC * sr))
    rng = np.random.default_rng(20260717)
    env = np.exp(-np.arange(n) / (CLICK_DECAY_SEC * sr))
    return (rng.standard_normal(n) * env).astype(np.float32)


def truth_click_times(bpm: float = BPM, n_beats: int = N_BEATS, start_sec: float = START_SEC) -> List[float]:
    spb = 60.0 / bpm
    return [round(start_sec + i * spb, 6) for i in range(n_beats)]


def generate_periodic_click_track(sr: int = SAMPLE_RATE, click_times_sec: Optional[List[float]] = None) -> np.ndarray:
    times = click_times_sec if click_times_sec is not None else truth_click_times()
    total_len = int(round((max(times) + 2.0) * sr))
    audio = np.zeros(total_len, dtype=np.float32)
    click = _percussive_click(sr)
    for t in times:
        c = int(round(t * sr))
        end = min(total_len, c + len(click))
        audio[c:end] += click[: end - c]
    peak = float(np.max(np.abs(audio)))
    if peak > 0:
        audio = audio / peak * 0.9
    return audio


def build_periodic_click_fixtures(
    out_dir: Path,
    ffmpeg_bin: Optional[str] = None,
    bpm: float = BPM,
    n_beats: int = N_BEATS,
) -> Dict[str, Path]:
    """Renders a periodic click track at `bpm`. Filenames carry the BPM so a
    174 BPM fixture (P3, BUG-229 follow-up) doesn't clobber the original 128
    BPM one — `periodic_click_track.*` is kept unsuffixed for the 128 BPM
    default to stay backward-compatible with anything already reading it."""
    times = truth_click_times(bpm=bpm, n_beats=n_beats)
    audio = generate_periodic_click_track(click_times_sec=times)
    suffix = "" if bpm == BPM else f"_{int(round(bpm))}bpm"
    wav_path = out_dir / f"periodic_click_track{suffix}.wav"
    write_wav(wav_path, audio)
    mp3_path = out_dir / f"periodic_click_track{suffix}.mp3"
    aac_path = out_dir / f"periodic_click_track{suffix}.m4a"
    encode_lossy(wav_path, mp3_path, "mp3", ffmpeg_bin)
    encode_lossy(wav_path, aac_path, "aac", ffmpeg_bin)
    return {"wav": wav_path, "mp3": mp3_path, "aac": aac_path}


@dataclass
class BeatTrackerAlignment:
    n_beats_detected: int
    n_truth: int
    # Absolute alignment to known click positions, edge beats excluded.
    n_matched_absolute: int
    median_abs_offset_ms: Optional[float]
    max_abs_offset_ms: Optional[float]
    # Cross-format: this format's beat times vs the wav reference's, matched
    # nearest-neighbor (the literal "does format shift the grid" gate).
    n_matched_vs_wav: Optional[int]
    max_abs_diff_vs_wav_ms: Optional[float]


def _match_nearest(pred: List[float], truth: List[float], window_sec: float) -> List[Optional[float]]:
    """For each truth time, the nearest pred time within window_sec, or None."""
    out: List[Optional[float]] = []
    for t in truth:
        if not pred:
            out.append(None)
            continue
        nearest = min(pred, key=lambda p: abs(p - t))
        out.append(nearest if abs(nearest - t) <= window_sec else None)
    return out


def measure_beat_tracker_alignment(
    fixtures: Dict[str, Path],
    truth_times: Optional[List[float]] = None,
    bpm: float = BPM,
    _detected_by_format: Optional[Dict[str, List[float]]] = None,
) -> Dict[str, BeatTrackerAlignment]:
    truth = truth_times if truth_times is not None else truth_click_times(bpm=bpm)
    spb = 60.0 / bpm
    window = spb * 0.5

    detected_by_format: Dict[str, List[float]]
    if _detected_by_format is not None:
        detected_by_format = _detected_by_format  # reuse an already-run inference (fitted-grid caller)
    else:
        detected_by_format = {}
        for fmt, path in fixtures.items():
            result = estimate_beats_this(str(path))
            detected_by_format[fmt] = result.beat_times if result is not None else []

    wav_beats = detected_by_format.get("wav", [])

    out: Dict[str, BeatTrackerAlignment] = {}
    for fmt, beats in detected_by_format.items():
        # Absolute alignment vs known truth, excluding edge beats.
        interior_truth = truth[EDGE_BEATS_EXCLUDED:-EDGE_BEATS_EXCLUDED] if len(truth) > 2 * EDGE_BEATS_EXCLUDED else truth
        matched = _match_nearest(beats, interior_truth, window)
        offsets_ms = [(m - t) * 1000.0 for m, t in zip(matched, interior_truth) if m is not None]

        # Cross-format vs wav.
        diff_ms: List[float] = None
        n_matched_wav = None
        if fmt != "wav" and wav_beats:
            matched_wav = _match_nearest(beats, wav_beats, window)
            pairs = [(m, w) for m, w in zip(matched_wav, wav_beats) if m is not None]
            if pairs:
                diff_ms = [(m - w) * 1000.0 for m, w in pairs]
                n_matched_wav = len(pairs)

        out[fmt] = BeatTrackerAlignment(
            n_beats_detected=len(beats),
            n_truth=len(truth),
            n_matched_absolute=len(offsets_ms),
            median_abs_offset_ms=float(np.median(np.abs(offsets_ms))) if offsets_ms else None,
            max_abs_offset_ms=float(np.max(np.abs(offsets_ms))) if offsets_ms else None,
            n_matched_vs_wav=n_matched_wav,
            max_abs_diff_vs_wav_ms=float(np.max(np.abs(diff_ms))) if diff_ms else (0.0 if fmt == "wav" else None),
        )
    return out


def fit_regular_grid_from_beats(
    beat_times: List[float],
    duration_sec: float,
    min_bpm: float = 55.0,
    max_bpm: float = 215.0,
) -> List[float]:
    """The P3/BUG-229 fitted-grid measurement: median inter-beat-interval ->
    one BPM number, then a phase anchor averaged over EVERY detected beat
    (not just one reference point, which would carry its own ~20ms
    quantization noise) -> `_build_regular_beat_grid` (manifold_audio.bpm),
    the exact primitive `_build_beat_grid()`'s own "synthetic" mode already
    uses to turn (bpm, anchor) into a regular grid over the whole track.
    Reused verbatim, not reimplemented."""
    beats = sorted(float(t) for t in beat_times if np.isfinite(t) and t >= 0.0)
    if len(beats) < 2:
        return []

    intervals = np.diff(np.asarray(beats, dtype=np.float64))
    intervals = intervals[intervals > 1e-6]
    if intervals.size == 0:
        return []
    # Median first (robust reference — throws out gross octave/missed-beat
    # errors, same role it plays elsewhere in bpm.py), THEN the mean of the
    # intervals that agree with it within 25% (quantization noise is
    # symmetric around the true interval; averaging many samples of it is
    # what actually cancels it out — a median alone can lock onto one side
    # of a near-50/50 bimodal split when the true interval sits close to a
    # frame-boundary half-step, e.g. 128 BPM against Beat This's 50fps grid:
    # 60/128/(1/50) = 23.4375 frames, almost exactly the worst case).
    median_ibi = float(np.median(intervals))
    if median_ibi <= 1e-6:
        return []
    agreeing = intervals[np.abs(intervals - median_ibi) <= 0.25 * median_ibi]
    mean_ibi = float(np.mean(agreeing)) if agreeing.size else median_ibi
    bpm = _normalize_bpm(60.0 / mean_ibi, min_bpm, max_bpm)
    if bpm is None:
        return []
    spb = 60.0 / bpm

    t0 = beats[0]
    # Average phase offset across all beats: each beat's residual from the
    # nearest multiple of spb relative to t0, averaged -> one anchor that
    # uses every beat's information instead of a single noisy sample.
    residuals = [(t - t0) - round((t - t0) / spb) * spb for t in beats]
    anchor_sec = t0 + float(np.mean(residuals))

    return _build_regular_beat_grid(duration_sec=duration_sec, bpm=bpm, anchor_sec=anchor_sec)


@dataclass
class FittedGridAlignment:
    fitted_bpm: Optional[float]
    n_grid_points: int
    n_truth: int
    n_matched: int
    median_abs_offset_ms: Optional[float]
    max_abs_offset_ms: Optional[float]


def measure_fitted_grid_alignment(
    fixtures: Dict[str, Path],
    truth_times: Optional[List[float]] = None,
    bpm: float = BPM,
    duration_sec: Optional[float] = None,
) -> Dict[str, FittedGridAlignment]:
    """Same detection pass as measure_beat_tracker_alignment, but scores the
    FITTED regular grid (fit_regular_grid_from_beats) against truth instead
    of the raw per-beat tracker output — the comparison point BUG-229's
    landing report asked P3 to produce."""
    truth = truth_times if truth_times is not None else truth_click_times(bpm=bpm)
    spb = 60.0 / bpm
    window = spb * 0.5
    dur = duration_sec if duration_sec is not None else (max(truth) + 2.0)

    out: Dict[str, FittedGridAlignment] = {}
    for fmt, path in fixtures.items():
        result = estimate_beats_this(str(path))
        raw_beats = result.beat_times if result is not None else []
        grid = fit_regular_grid_from_beats(raw_beats, duration_sec=dur)

        fitted_bpm = None
        if len(raw_beats) >= 2:
            ibis = np.diff(np.asarray(sorted(raw_beats), dtype=np.float64))
            ibis = ibis[ibis > 1e-6]
            if ibis.size:
                fitted_bpm = _normalize_bpm(60.0 / float(np.median(ibis)))

        interior_truth = truth[EDGE_BEATS_EXCLUDED:-EDGE_BEATS_EXCLUDED] if len(truth) > 2 * EDGE_BEATS_EXCLUDED else truth
        matched = _match_nearest(grid, interior_truth, window)
        offsets_ms = [(m - t) * 1000.0 for m, t in zip(matched, interior_truth) if m is not None]

        out[fmt] = FittedGridAlignment(
            fitted_bpm=fitted_bpm,
            n_grid_points=len(grid),
            n_truth=len(interior_truth),
            n_matched=len(offsets_ms),
            median_abs_offset_ms=float(np.median(np.abs(offsets_ms))) if offsets_ms else None,
            max_abs_offset_ms=float(np.max(np.abs(offsets_ms))) if offsets_ms else None,
        )
    return out


def main(argv: Optional[List[str]] = None) -> int:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", type=Path, default=Path("eval/data/click_track_fixtures"))
    parser.add_argument("--report", type=Path, default=Path("eval/scoreboard/d14_beat_tracker_alignment_report.json"))
    parser.add_argument("--ffmpeg-bin", type=str, default=None)
    parser.add_argument(
        "--bpms",
        type=float,
        nargs="+",
        default=[128.0, 174.0],
        help="periodic click BPMs to measure (P3/BUG-229: 128 was P2's fixture, 174 is P3's added electronic-domain tempo)",
    )
    args = parser.parse_args(argv)

    report: Dict[str, Dict] = {}
    for bpm in args.bpms:
        fixtures = build_periodic_click_fixtures(args.out_dir, args.ffmpeg_bin, bpm=bpm)
        raw = measure_beat_tracker_alignment(fixtures, bpm=bpm)
        fitted = measure_fitted_grid_alignment(fixtures, bpm=bpm)
        report[f"{int(round(bpm))}bpm"] = {
            "raw": {fmt: asdict(a) for fmt, a in raw.items()},
            "fitted_grid": {fmt: asdict(a) for fmt, a in fitted.items()},
        }

    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2))
    print(json.dumps(report, indent=2))
    print(f"[beat_tracker_alignment] wrote {args.report}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
