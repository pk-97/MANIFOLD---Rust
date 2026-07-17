"""D14 click-track tests. Covers the sign convention (the easiest place for
this module to silently do the wrong thing) and the fixture-generation
round-trip, without requiring ffmpeg/mp3/AAC (those are covered by the real
gate run, `eval/scoreboard/d14_alignment_report.json`, which needs ffmpeg)."""

from __future__ import annotations

import numpy as np

from eval.click_track import CLICK_TIMES_SEC, generate_click_track, measure_decode_stage_offset, write_wav
from manifold_audio.audio_io import _shift_audio_by_correction


def test_generate_click_track_places_bursts_at_exact_centers(tmp_path):
    times = [1.0, 2.0, 3.0]
    audio = generate_click_track(sr=44100, click_times_sec=times)
    # Each expected center should be within a couple samples of a local peak.
    for t in times:
        center = int(round(t * 44100))
        window = audio[max(0, center - 50) : center + 50]
        assert np.max(np.abs(window)) > 0.1


def test_measure_decode_stage_offset_is_near_zero_for_wav(tmp_path):
    audio = generate_click_track(sr=44100, click_times_sec=[1.0, 2.0, 3.0])
    wav_path = tmp_path / "clicks.wav"
    write_wav(wav_path, audio)
    offset = measure_decode_stage_offset(wav_path, [1.0, 2.0, 3.0])
    assert offset.n_matched == 3
    # wav goes through _read_wav_to_mono_float directly (no ffmpeg round
    # trip) -> should be a small fraction of a sample, not tens of ms.
    assert abs(offset.median_offset_ms) < 1.0


def test_sign_convention_matches_between_click_track_and_audio_io():
    """The single most dangerous place for D14 to silently do the wrong
    thing: click_track.py computes correction_sec with one sign convention,
    audio_io.py's _apply_decode_stage_correction must consume it with the
    SAME one. This test locks the contract at the function-call level
    rather than relying on end-to-end agreement."""
    shifted = _shift_audio_by_correction(
        audio=np.arange(100, dtype=np.float32), sr=100, correction_sec=0.05  # +50ms = "late" -> trim 5 samples from start
    )
    assert len(shifted) == 95
    assert shifted[0] == 5.0  # first 5 samples (the "late" padding) trimmed

    shifted_early = _shift_audio_by_correction(
        audio=np.arange(100, dtype=np.float32), sr=100, correction_sec=-0.05  # -50ms = "early" -> pad 5 zero samples at start
    )
    assert len(shifted_early) == 105
    assert shifted_early[0] == 0.0
    assert shifted_early[5] == 0.0  # original sample 0, now at index 5
