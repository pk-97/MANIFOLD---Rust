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
from manifold_audio.beat_tracking import estimate_beats_this  # noqa: E402

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


def build_periodic_click_fixtures(out_dir: Path, ffmpeg_bin: Optional[str] = None) -> Dict[str, Path]:
    audio = generate_periodic_click_track()
    wav_path = out_dir / "periodic_click_track.wav"
    write_wav(wav_path, audio)
    mp3_path = out_dir / "periodic_click_track.mp3"
    aac_path = out_dir / "periodic_click_track.m4a"
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
) -> Dict[str, BeatTrackerAlignment]:
    truth = truth_times if truth_times is not None else truth_click_times()
    spb = 60.0 / BPM
    window = spb * 0.5

    detected_by_format: Dict[str, List[float]] = {}
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


def main(argv: Optional[List[str]] = None) -> int:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", type=Path, default=Path("eval/data/click_track_fixtures"))
    parser.add_argument("--report", type=Path, default=Path("eval/scoreboard/d14_beat_tracker_alignment_report.json"))
    parser.add_argument("--ffmpeg-bin", type=str, default=None)
    args = parser.parse_args(argv)

    fixtures = build_periodic_click_fixtures(args.out_dir, args.ffmpeg_bin)
    alignment = measure_beat_tracker_alignment(fixtures)

    report = {fmt: asdict(a) for fmt, a in alignment.items()}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2))
    print(json.dumps(report, indent=2))
    print(f"[beat_tracker_alignment] wrote {args.report}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
