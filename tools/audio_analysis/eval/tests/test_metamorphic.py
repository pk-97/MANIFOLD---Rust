"""Metamorphic suite tests — using a synthetic click-track-like signal so
these run in milliseconds and don't depend on any fetched dataset. The
babyslakh-backed gate (`python -m eval.run --set dev`) exercises the same
functions against real audio."""

from __future__ import annotations

import numpy as np

from eval.metamorphic import (
    DetectorOutput,
    check_gain_invariance,
    check_noise_floor_silence,
    check_stem_injection,
    check_time_stretch_invariance,
    run_metamorphic_suite,
)

SR = 44100


def _synthetic_clicks(times_sec, sr=SR, duration_sec=5.0):
    n = int(duration_sec * sr)
    audio = np.zeros(n, dtype=np.float32)
    for t in times_sec:
        idx = int(round(t * sr))
        if 0 <= idx < n - 10:
            audio[idx : idx + 10] = 1.0
    return audio


def _threshold_detect_fn(threshold: float = 0.5):
    """A trivial deterministic 'detector': fixed-threshold peak crossings,
    scaled bpm from a fixed base (128) so gain/time-stretch invariance is
    exactly testable without any real model."""

    def detect(audio: np.ndarray, sr: int) -> DetectorOutput:
        above = np.abs(audio) > threshold
        edges = np.where(np.diff(above.astype(int)) == 1)[0]
        times = [(idx + 1) / sr for idx in edges]
        return DetectorOutput(event_times_sec=times, bpm=128.0)

    return detect


def test_gain_invariance_passes_for_stable_detector():
    audio = _synthetic_clicks([1.0, 2.0, 3.0])
    result = check_gain_invariance(audio, SR, _threshold_detect_fn())
    assert result.passed, result.detail


def test_gain_invariance_fails_when_gain_changes_event_count():
    audio = _synthetic_clicks([1.0, 2.0, 3.0])

    def flaky_detect(a, sr):
        # "Detector" that drops events under negative gain — a real bug shape.
        # Threshold 0.6: base peak 1.0 stays above it; -6dB attenuates to
        # ~0.501, dropping below (the click amplitude is 1.0, so +6dB clips
        # back to 1.0 and doesn't exercise this path — -6dB is the one that
        # must trip the count-tolerance check).
        peak = float(np.max(np.abs(a))) if len(a) else 0.0
        n = 3 if peak > 0.6 else 0
        return DetectorOutput(event_times_sec=[float(i) for i in range(n)], bpm=128.0)

    result = check_gain_invariance(audio, SR, flaky_detect, gain_db=6.0)
    assert not result.passed


def test_noise_floor_silence_passes_when_detector_ignores_noise():
    audio = np.zeros(int(2.5 * SR), dtype=np.float32)
    result = check_noise_floor_silence(audio, SR, _threshold_detect_fn(threshold=0.9), noise_db=-40.0)
    assert result.passed, result.detail


def test_noise_floor_silence_fails_when_detector_is_too_sensitive():
    audio = np.zeros(int(2.5 * SR), dtype=np.float32)
    # Threshold near zero -> any noise crosses it -> spurious "events".
    result = check_noise_floor_silence(audio, SR, _threshold_detect_fn(threshold=0.001), noise_db=-40.0)
    assert not result.passed


def test_stem_injection_detects_new_event_at_offset():
    audio = _synthetic_clicks([1.0])
    stem = _synthetic_clicks([0.0], duration_sec=0.5)  # a click at t=0 of the injected snippet
    result = check_stem_injection(audio, SR, stem, offset_sec=3.0, detect_fn=_threshold_detect_fn())
    assert result.passed, result.detail


def test_time_stretch_invariance_scales_bpm_and_times():
    audio = _synthetic_clicks([1.0, 2.0, 3.0])

    def scaling_detect(a, sr):
        # A detector whose bpm/times genuinely scale with playback rate,
        # inferred from duration relative to the original 5s buffer.
        rate = 5.0 * sr / len(a) if len(a) else 1.0
        above = np.abs(a) > 0.5
        edges = np.where(np.diff(above.astype(int)) == 1)[0]
        times = [(idx + 1) / sr for idx in edges]
        return DetectorOutput(event_times_sec=times, bpm=128.0 * rate)

    result = check_time_stretch_invariance(audio, SR, scaling_detect)
    assert result.passed, result.detail


def test_run_metamorphic_suite_returns_all_checks_without_injection():
    audio = _synthetic_clicks([1.0, 2.0, 3.0])
    results = run_metamorphic_suite(audio, SR, _threshold_detect_fn())
    names = {r.name for r in results}
    assert "gain_invariance" in names
    assert "noise_floor_silence" in names
    assert not any("stem_injection" in n for n in names)  # skipped, no stem supplied
