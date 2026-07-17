"""D14 beat-tracker alignment fixture tests. Covers the periodic click-track
generator and truth-time construction without running Beat This (model
inference is exercised by the real gate run,
eval/scoreboard/d14_beat_tracker_alignment_report.json)."""

from __future__ import annotations

import numpy as np

from eval.beat_tracker_alignment import BPM, generate_periodic_click_track, truth_click_times


def test_truth_click_times_are_evenly_spaced_at_bpm():
    times = truth_click_times(bpm=128.0, n_beats=8, start_sec=0.5)
    spb = 60.0 / 128.0
    assert len(times) == 8
    for i, t in enumerate(times):
        assert abs(t - (0.5 + i * spb)) < 1e-6


def test_generate_periodic_click_track_places_energy_at_each_click():
    times = truth_click_times(bpm=BPM, n_beats=6, start_sec=0.5)
    audio = generate_periodic_click_track(sr=44100, click_times_sec=times)
    for t in times:
        center = int(round(t * 44100))
        window = audio[max(0, center - 5) : center + 200]
        assert np.max(np.abs(window)) > 0.1
    # Peak-normalized, never clipping.
    assert np.max(np.abs(audio)) <= 0.9 + 1e-6
