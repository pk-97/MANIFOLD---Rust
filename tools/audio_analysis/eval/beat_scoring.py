"""P2 beat/downbeat scoring against real ground truth (AUDIO_ANALYSIS_ACCURACY_DESIGN.md
P2 gate: "beat F1 + downbeat F1 >= madmom baseline - noise floor on dev AND
heldout").

Ground truth source: the live-show corpus (Addendum 2026-07-17). P1's
liveshow_extract.py already pulled the project's tempoMap into
eval/liveshow_labels/grid_truth.json (97 points, <=2ms per Peter — the set
is grid-locked). This module reuses that same beat->seconds interpolation
(eval.liveshow_extract.beats_to_seconds) to generate a ground-truth beat time
at every integer beat within a fixture's beat_range, and a downbeat time
every 4th beat — all `beat_range` boundaries in fixtures.toml are multiples
of 4, confirmed 2026-07-17, so `(beat_num - start_beat) % 4 == 0` gives the
correct downbeat phase without re-deriving one.

Audio source: the master export referenced by fixtures.toml's
`fixture_common.liveshow` comment — resolved here from the local Dropbox
path (not committed; never redistributed, D8). Each fixture's segment is
sliced directly from the master (soundfile, sample-accurate) and written to
a gitignored temp wav under eval/data/ — beat tracking (old madmom arm, new
Beat This) always operates on the full mix, never demucs stems, so no
separation is needed for this comparison.

This is a BEFORE/AFTER harness: it can run against either the current
manifold_audio package or an isolated pre-edit copy (via PYTHONPATH,
run from that copy's own directory — see docs/landings/ for the invocation
used to produce the P2 before scoreboard). It does not import analyzer.py
or bpm.py by fixed path; it uses whatever `manifold_audio` resolves to in
sys.path, which is what makes the same script usable for both runs.
"""

from __future__ import annotations

import json
import sys
import tempfile
from pathlib import Path
from typing import Dict, List, Optional, Tuple

import numpy as np
import soundfile as sf

# Deliberately no sys.path.insert here (unlike run.py) — this module must be
# runnable against an isolated pre-edit copy of manifold_audio for the P2
# before/after scoreboard (see docs/landings/ for the invocation): the
# "before" run symlinks eval/ into a directory containing only the old
# manifold_audio and is invoked with that directory as cwd via
# `python -m eval.beat_scoring`, so both `eval` and `manifold_audio` resolve
# from cwd with no path surgery needed. Forcing this file's own directory
# onto sys.path here would always win over that arrangement and silently
# re-resolve manifold_audio to whichever copy sits next to this file.

from eval import metrics  # noqa: E402
from eval.liveshow_extract import TempoPoint, beats_to_seconds  # noqa: E402

MASTER_WAV = Path(
    "/Users/peterkiemann/Library/CloudStorage/Dropbox/Music Production/"
    "Ableton Projects/Other And Backups/Live Show 2026 Project/Samples/"
    "Recorded/29 MASTER 0012 [2026-04-26 161410].wav"
)
GRID_TRUTH_PATH = Path(__file__).resolve().parent / "liveshow_labels" / "grid_truth.json"
BEATS_PER_BAR = 4


def load_tempo_points(path: Path = GRID_TRUTH_PATH) -> List[TempoPoint]:
    data = json.loads(path.read_text())
    return [TempoPoint(**p) for p in data["points"]]


def ground_truth_beats(
    tempo_points: List[TempoPoint],
    beat_range: Tuple[float, float],
    beats_per_bar: int = BEATS_PER_BAR,
) -> Tuple[List[float], List[float], float, float]:
    """Returns (beat_times_sec, downbeat_times_sec, seg_start_sec, seg_end_sec)
    — beat/downbeat times relative to the segment start (seconds into the
    sliced audio), one entry per integer beat in [start_beat, end_beat)."""
    start_beat, end_beat = beat_range
    seg_start_sec = beats_to_seconds(start_beat, tempo_points)
    seg_end_sec = beats_to_seconds(end_beat, tempo_points)
    if seg_start_sec is None or seg_end_sec is None:
        raise ValueError(f"beat_range {beat_range} falls outside the recorded tempo map")

    n_beats = int(round(end_beat - start_beat))
    beat_times: List[float] = []
    downbeat_times: List[float] = []
    for i in range(n_beats):
        beat_num = start_beat + i
        t = beats_to_seconds(beat_num, tempo_points)
        if t is None:
            continue
        rel = t - seg_start_sec
        beat_times.append(rel)
        if i % beats_per_bar == 0:
            downbeat_times.append(rel)
    return beat_times, downbeat_times, seg_start_sec, seg_end_sec


def slice_master_to_wav(
    seg_start_sec: float,
    seg_end_sec: float,
    out_path: Path,
    master_wav: Path = MASTER_WAV,
    pad_sec: float = 0.5,
) -> None:
    """Sample-accurate slice of the master export into its own mono wav.
    A small symmetric pad is included (clamped to the file bounds) so the
    beat tracker sees real context at the segment edges rather than a hard
    cut exactly on beat 0 — ground-truth times are NOT shifted; they stay
    relative to seg_start_sec, and the pad is subtracted back out again
    below by the caller (see build_song_fixture)."""
    info = sf.info(str(master_wav))
    sr = info.samplerate
    start_frame = max(0, int(round((seg_start_sec - pad_sec) * sr)))
    end_frame = min(info.frames, int(round((seg_end_sec + pad_sec) * sr)))
    audio, read_sr = sf.read(str(master_wav), start=start_frame, frames=end_frame - start_frame, always_2d=True)
    mono = audio.mean(axis=1).astype(np.float32)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    sf.write(str(out_path), mono, read_sr, subtype="PCM_16")


def build_song_fixture(
    fixture: Dict[str, object],
    tempo_points: List[TempoPoint],
    out_dir: Path,
    pad_sec: float = 0.5,
) -> Dict[str, object]:
    """Slices one liveshow song fixture to a wav + ground truth, returns a
    dict with paths, truth beat/downbeat times (relative to the UNPADDED
    segment start), and the pad offset applied (so callers can shift
    detector output back into the same relative-time frame)."""
    beat_range = tuple(fixture["beat_range"])  # type: ignore[arg-type]
    beat_times, downbeat_times, seg_start_sec, seg_end_sec = ground_truth_beats(tempo_points, beat_range)
    wav_path = out_dir / f"{fixture['id']}.wav"
    slice_master_to_wav(seg_start_sec, seg_end_sec, wav_path, pad_sec=pad_sec)
    return {
        "id": fixture["id"],
        "split": fixture["split"],
        "domain": fixture.get("domain", "other"),
        "wav_path": wav_path,
        "truth_beat_times_sec": beat_times,
        "truth_downbeat_times_sec": downbeat_times,
        "pad_sec": pad_sec,
        "seg_start_sec": seg_start_sec,
        "seg_end_sec": seg_end_sec,
    }


def run_beat_tracker_on_fixture(fixture: Dict[str, object]) -> Dict[str, object]:
    """Runs the CURRENT manifold_audio package's analyze_percussion() on the
    fixture's sliced wav (drums-only instrument scope — cheapest ADTOF pass;
    beat tracking runs regardless of which instruments are requested) and
    returns detected beat/downbeat times shifted back into the unpadded
    segment's relative-time frame."""
    from manifold_audio.analyzer import analyze_percussion
    from manifold_audio.audio_io import load_audio_mono

    wav_path = fixture["wav_path"]
    pad_sec = fixture["pad_sec"]
    audio, sr = load_audio_mono(Path(wav_path), target_sr=44100, ffmpeg_bin=None)
    events, bpm, bpm_conf, beat_grid, *_ = analyze_percussion(
        audio=audio,
        sample_rate=sr,
        frame_size=1024,
        hop_size=256,
        profile_name="electronic",
        emit_bass=False,
        audio_path=str(wav_path),
        analysis_audio_path=str(wav_path),
        min_bpm=55.0,
        max_bpm=215.0,
        instruments=frozenset({"drums"}),
    )
    pred_beats: List[float] = []
    pred_downbeats: List[float] = []
    tracker = "none"
    if beat_grid is not None:
        tracker = getattr(beat_grid, "tracker", beat_grid.mode)
        pred_beats = [t - pad_sec for t in beat_grid.beat_times]
        pred_downbeats = [pred_beats[i] for i in beat_grid.downbeat_indices if 0 <= i < len(pred_beats)]
    return {
        "id": fixture["id"],
        "tracker": tracker,
        "bpm": bpm,
        "pred_beat_times_sec": pred_beats,
        "pred_downbeat_times_sec": pred_downbeats,
    }


def score_fixture(fixture: Dict[str, object], pred: Dict[str, object]) -> Dict[str, object]:
    beat_prf = metrics.beat_prf(pred["pred_beat_times_sec"], fixture["truth_beat_times_sec"])
    downbeat_prf = metrics.downbeat_prf(pred["pred_downbeat_times_sec"], fixture["truth_downbeat_times_sec"])
    return {
        "id": fixture["id"],
        "split": fixture["split"],
        "domain": fixture["domain"],
        "tracker": pred["tracker"],
        "bpm": pred["bpm"],
        "n_truth_beats": len(fixture["truth_beat_times_sec"]),
        "n_pred_beats": len(pred["pred_beat_times_sec"]),
        "beat_f1": beat_prf.f1,
        "beat_precision": beat_prf.precision,
        "beat_recall": beat_prf.recall,
        "n_truth_downbeats": len(fixture["truth_downbeat_times_sec"]),
        "n_pred_downbeats": len(pred["pred_downbeat_times_sec"]),
        "downbeat_f1": downbeat_prf.f1,
        "downbeat_precision": downbeat_prf.precision,
        "downbeat_recall": downbeat_prf.recall,
    }


LIVESHOW_SONG_FIXTURES = [
    {"id": "liveshow_midnight_patience", "split": "dev", "domain": "electronic", "beat_range": [128.0, 640.0]},
    {"id": "liveshow_integer", "split": "dev", "domain": "electronic", "beat_range": [640.0, 1088.0]},
    {"id": "liveshow_pattern", "split": "dev", "domain": "electronic", "beat_range": [1088.0, 1280.0]},
    {"id": "liveshow_stagnate", "split": "heldout", "domain": "electronic", "beat_range": [1280.0, 1728.0]},
    {"id": "liveshow_basalt", "split": "heldout", "domain": "electronic", "beat_range": [1728.0, 2176.0]},
    {"id": "liveshow_all_in_for_you", "split": "dev", "domain": "electronic", "beat_range": [2176.0, 2416.0]},
    {"id": "liveshow_oh_so_suddenly", "split": "dev", "domain": "electronic", "beat_range": [2416.0, 3104.0]},
]


def main(argv: Optional[List[str]] = None) -> int:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", type=Path, default=Path("eval/data/liveshow_song_slices"))
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--label", type=str, required=True, help="e.g. 'before' or 'after' — recorded in the report")
    args = parser.parse_args(argv)

    tempo_points = load_tempo_points()
    args.out_dir.mkdir(parents=True, exist_ok=True)

    rows = []
    for fx in LIVESHOW_SONG_FIXTURES:
        print(f"[beat_scoring] {fx['id']} ...", file=sys.stderr)
        fixture = build_song_fixture(fx, tempo_points, args.out_dir)
        pred = run_beat_tracker_on_fixture(fixture)
        rows.append(score_fixture(fixture, pred))

    by_split: Dict[str, Dict[str, Dict[str, float]]] = {}
    for split in ("dev", "heldout"):
        split_rows = [r for r in rows if r["split"] == split]
        by_split[split] = {
            "beat_f1": metrics.domain_aggregate(split_rows, "beat_f1"),
            "downbeat_f1": metrics.domain_aggregate(split_rows, "downbeat_f1"),
        }

    report = {"label": args.label, "rows": rows, "by_split": by_split}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2))
    print(f"[beat_scoring] wrote {args.report}")
    for split in ("dev", "heldout"):
        print(
            f"  {split}: beat_f1={by_split[split]['beat_f1']['overall']['mean']:.4f} "
            f"downbeat_f1={by_split[split]['downbeat_f1']['overall']['mean']:.4f} "
            f"(n={by_split[split]['beat_f1']['overall']['n']})"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
