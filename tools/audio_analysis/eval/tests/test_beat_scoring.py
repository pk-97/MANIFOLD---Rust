"""P2 beat/downbeat ground-truth extraction tests. Covers the pure-function
part of eval/beat_scoring.py (tempo-map integration, downbeat phase) without
touching the master audio or running any model — those are exercised by the
real P2 scoreboard runs (eval/scoreboard/p2_beat_*.json)."""

from __future__ import annotations

from eval.beat_scoring import ground_truth_beats
from eval.liveshow_extract import TempoPoint


def _constant_tempo_points(bpm: float, start_beat: float = 0.0) -> list:
    """A two-point tempo map at a fixed BPM, matching grid_truth.json's
    shape (beat, bpm, recorded_at_seconds, source)."""
    spb = 60.0 / bpm
    return [
        TempoPoint(beat=start_beat, bpm=bpm, recorded_at_seconds=0.0, source=4),
        TempoPoint(beat=start_beat + 1000.0, bpm=bpm, recorded_at_seconds=1000.0 * spb, source=4),
    ]


def test_ground_truth_beats_spacing_matches_bpm():
    points = _constant_tempo_points(bpm=120.0)
    beat_times, downbeat_times, seg_start, seg_end = ground_truth_beats(points, (0.0, 16.0))
    assert len(beat_times) == 16
    spb = 60.0 / 120.0
    for i, t in enumerate(beat_times):
        assert abs(t - i * spb) < 1e-6
    # First beat of the segment is relative time 0.
    assert beat_times[0] == 0.0


def test_ground_truth_downbeats_every_fourth_beat():
    points = _constant_tempo_points(bpm=128.0)
    beat_times, downbeat_times, _, _ = ground_truth_beats(points, (128.0, 640.0))
    assert len(downbeat_times) == len(beat_times) / 4
    # Downbeats are exactly every 4th beat, starting at the segment's first beat.
    assert downbeat_times == beat_times[0::4]


def test_ground_truth_beats_relative_to_segment_start_not_absolute():
    """beat_range not starting at project beat 0 must still produce
    beat_times relative to the SEGMENT's own start — a common off-by-offset
    bug for any beat->seconds slice."""
    points = _constant_tempo_points(bpm=132.0)
    beat_times, _, seg_start_sec, _ = ground_truth_beats(points, (640.0, 648.0))
    spb = 60.0 / 132.0
    assert seg_start_sec > 0.0  # the segment starts well into the track
    assert beat_times[0] == 0.0
    assert abs(beat_times[1] - spb) < 1e-6
