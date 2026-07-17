"""D14 beat-tracker alignment fixture tests. Covers the periodic click-track
generator and truth-time construction without running Beat This (model
inference is exercised by the real gate run,
eval/scoreboard/d14_beat_tracker_alignment_report.json)."""

from __future__ import annotations

import numpy as np

from eval.beat_tracker_alignment import (
    BPM,
    fit_regular_grid_from_beats,
    generate_periodic_click_track,
    truth_click_times,
)


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


def test_truth_click_times_supports_arbitrary_bpm_e_g_174():
    # P3/BUG-229 follow-up added a 174 BPM fixture alongside P2's 128 BPM one
    # — the generator already parameterizes on bpm, no new code needed.
    times = truth_click_times(bpm=174.0, n_beats=10, start_sec=0.75)
    spb = 60.0 / 174.0
    assert len(times) == 10
    for i, t in enumerate(times):
        assert abs(t - (0.75 + i * spb)) < 1e-6


def _quantize_to_frame_rate(times, fps=50.0):
    """Simulates Beat This's 50fps (20ms) frame quantization: each true time
    snaps to the nearest frame boundary."""
    frame = 1.0 / fps
    return [round(t / frame) * frame for t in times]


def test_fit_regular_grid_averages_out_frame_quantization_noise():
    """The P3/BUG-229 hypothesis, isolated from model inference: a regular
    grid fitted from median IBI + an all-beats-averaged anchor should land
    much closer to the true click positions than the raw quantized beats
    do, because per-beat quantization error (uniform in [-10ms, 10ms] at
    50fps) averages toward zero across many beats instead of being scored
    one at a time."""
    bpm = 128.0
    truth = truth_click_times(bpm=bpm, n_beats=64, start_sec=0.75)
    quantized = _quantize_to_frame_rate(truth, fps=50.0)

    raw_offsets_ms = [abs(q - t) * 1000.0 for q, t in zip(quantized, truth)]
    raw_median_ms = float(np.median(raw_offsets_ms))

    duration_sec = truth[-1] + 2.0
    grid = fit_regular_grid_from_beats(quantized, duration_sec=duration_sec)
    assert len(grid) > 0

    # Match each truth time to nearest fitted-grid point.
    fitted_offsets_ms = []
    for t in truth:
        nearest = min(grid, key=lambda g: abs(g - t))
        fitted_offsets_ms.append(abs(nearest - t) * 1000.0)
    fitted_median_ms = float(np.median(fitted_offsets_ms))

    # The fitted grid must not be worse than the raw quantized alignment,
    # and on this clean synthetic case (no detector error beyond frame
    # quantization) it should land comfortably under the D14 5ms target.
    assert fitted_median_ms <= raw_median_ms
    assert fitted_median_ms < 5.0


def test_fit_regular_grid_returns_empty_on_insufficient_beats():
    assert fit_regular_grid_from_beats([1.0], duration_sec=10.0) == []
    assert fit_regular_grid_from_beats([], duration_sec=10.0) == []
