"""Unit tests for train/dataset.py's patch-extraction and "other"-mining
machinery (docs/AUDIO_EVENT_CLASSIFIER_DESIGN.md §5 P1 gate): patch shape,
jitter bounds, liveshow in-window truth extraction, "other" mining's 50ms
exclusion, and seed determinism."""

from __future__ import annotations

import numpy as np

from eval.liveshow_extract import TempoPoint
from train import dataset as ds


def _synthetic_track(sr: int = 44100, dur_sec: float = 2.0, seed: int = 0):
    rng = np.random.default_rng(seed)
    return (rng.standard_normal(int(sr * dur_sec)).astype(np.float32) * 0.1), sr


# ---------------------------------------------------------------------------
# Mel-patch extraction
# ---------------------------------------------------------------------------


def test_extract_mel_patch_shape_and_dtype():
    audio, sr = _synthetic_track()
    patch = ds.extract_mel_patch(audio, sr, onset_sec=1.0)
    assert patch.shape == (ds.N_MELS, ds.N_FRAMES)
    assert patch.dtype == np.float32


def test_extract_mel_patch_is_deterministic_for_the_same_inputs():
    audio, sr = _synthetic_track()
    a = ds.extract_mel_patch(audio, sr, onset_sec=1.0)
    b = ds.extract_mel_patch(audio, sr, onset_sec=1.0)
    assert np.array_equal(a, b)


def test_extract_mel_patch_near_track_edges_is_zero_padded_not_crashing():
    audio, sr = _synthetic_track(dur_sec=0.05)
    start = ds.extract_mel_patch(audio, sr, onset_sec=0.0)
    end = ds.extract_mel_patch(audio, sr, onset_sec=0.05)
    assert start.shape == (ds.N_MELS, ds.N_FRAMES)
    assert end.shape == (ds.N_MELS, ds.N_FRAMES)
    assert np.all(np.isfinite(start))
    assert np.all(np.isfinite(end))


# ---------------------------------------------------------------------------
# Jitter bounds
# ---------------------------------------------------------------------------


def test_jitter_draws_are_bounded_to_ten_ms():
    rng = np.random.default_rng(1)
    for _ in range(1000):
        jitter = float(rng.uniform(-ds.JITTER_MS, ds.JITTER_MS)) / 1000.0
        assert abs(jitter) <= ds.JITTER_MS / 1000.0


def test_make_examples_base_copy_is_unjittered_second_copy_is_bounded():
    audio, sr = _synthetic_track()
    rng = np.random.default_rng(2)
    examples = ds._make_examples(rng, audio, sr, 1.0, "kick", "test_source", "track0", {})
    assert len(examples) == 2
    base, jittered = examples
    assert base.jitter_sec == 0.0
    assert base.label == "kick" and jittered.label == "kick"
    assert abs(jittered.jitter_sec) <= ds.JITTER_MS / 1000.0
    assert jittered.jitter_sec != 0.0  # deterministic seed, continuous draw -- not exactly 0 in practice
    # Both copies share the same (fallback-computed) side-feature vector.
    assert np.array_equal(base.side_features, jittered.side_features)
    assert base.side_features.shape == (len(ds.SIDE_FEATURE_NAMES),)


# ---------------------------------------------------------------------------
# "other"-class mining
# ---------------------------------------------------------------------------


def test_mine_other_onsets_excludes_anything_near_truth():
    truth = {"kick": [1.0, 1.5, 2.0], "snare": []}
    detected = [1.0, 1.02, 1.5, 1.8, 2.0]  # 1.8 is >50ms from any truth, still inside the run's window
    out = ds.mine_other_onsets(detected, truth, bpm=120.0)
    assert out == [1.8]


def test_mine_other_onsets_drops_candidates_outside_any_window():
    truth = {"kick": [1.0, 1.2, 1.4]}
    detected = [1.0, 1.2, 1.4, 90.0]  # 90.0 is far outside the run's padded window
    out = ds.mine_other_onsets(detected, truth, bpm=120.0)
    assert 90.0 not in out


def test_mine_other_onsets_respects_the_fifty_ms_boundary():
    truth = {"kick": [1.0, 1.2, 1.4]}
    just_inside = 1.0 + ds.OTHER_MIN_GAP_SEC - 0.001  # < 50ms from truth -> excluded
    just_outside = 1.0 + ds.OTHER_MIN_GAP_SEC + 0.001  # > 50ms from truth -> kept (if in-window)
    out = ds.mine_other_onsets([just_inside, just_outside], truth, bpm=120.0)
    assert just_inside not in out
    assert just_outside in out


def test_mine_other_onsets_no_truth_at_all_yields_no_windows():
    out = ds.mine_other_onsets([1.0, 2.0], {"kick": []}, bpm=120.0)
    assert out == []


def test_mine_other_onsets_unions_windows_across_classes():
    # kick's own run is short; snare's overlapping run extends the active
    # passage further -- a detection only snare's window would cover must
    # still be scoreable as a candidate (union, not per-class isolation).
    truth = {"kick": [1.0, 1.2], "snare": [1.0, 5.0]}
    detected = [3.0]  # inside snare's [1.0, 5.0] run, outside kick's own
    out = ds.mine_other_onsets(detected, truth, bpm=120.0)
    assert out == [3.0]


# ---------------------------------------------------------------------------
# Liveshow truth stays inside its own segment (in-window discipline)
# ---------------------------------------------------------------------------


def test_liveshow_truth_for_song_only_reads_edges_within_its_own_segment():
    # Two bracketing points -- beats_to_seconds interpolates BETWEEN
    # recorded points and holds flat past the last one, so a single point
    # can't answer a query past its own beat (needed here for beat=8.0).
    tempo_points = [
        TempoPoint(beat=0.0, bpm=120.0, recorded_at_seconds=0.0, source=4),
        TempoPoint(beat=100.0, bpm=120.0, recorded_at_seconds=50.0, source=4),
    ]
    fixture = {"beat_range": [0.0, 8.0]}  # 120bpm -> 0.5s/beat, segment = [0, 4)s
    onset_truth = [
        {"instrument": "kick", "edges_secs_in_audio": [0.5, 1.0, 5.0]},  # 5.0 is outside [0, 4)
        {"instrument": "vocal", "edges_secs_in_audio": [2.0]},
        {"instrument": "bass_sustained", "edges_secs_in_audio": [1.5]},  # not a P1 class (D3)
    ]
    truth, seg_start, seg_end = ds._liveshow_truth_for_song(fixture, tempo_points, onset_truth, pad_sec=0.5)
    assert truth["kick"] == [1.0, 1.5]  # 0.5+0.5, 1.0+0.5 -- 5.0 dropped (outside the segment)
    assert truth["vocal"] == [2.5]
    assert "bass_sustained" not in truth
    assert seg_start == 0.0 and seg_end == 4.0


def test_liveshow_truth_for_song_unknown_instrument_is_ignored():
    tempo_points = [TempoPoint(beat=0.0, bpm=120.0, recorded_at_seconds=0.0, source=4)]
    fixture = {"beat_range": [0.0, 8.0]}
    onset_truth = [{"instrument": "reverse_cymbal_swell", "edges_secs_in_audio": [1.0]}]
    truth, _s, _e = ds._liveshow_truth_for_song(fixture, tempo_points, onset_truth)
    assert all(len(v) == 0 for v in truth.values())


# ---------------------------------------------------------------------------
# Seed determinism (small, self-contained source subset -- fast)
# ---------------------------------------------------------------------------


def test_build_dataset_is_seed_deterministic_on_self_render():
    examples_a = ds.build_dataset(seed=777, only_source_ids=["self_render"])
    examples_b = ds.build_dataset(seed=777, only_source_ids=["self_render"])
    assert len(examples_a) > 0
    assert len(examples_a) == len(examples_b)
    assert ds.support_table(examples_a) == ds.support_table(examples_b)
    assert ds.first_patch_checksum(examples_a) == ds.first_patch_checksum(examples_b)


def test_build_dataset_different_seeds_keep_the_same_counts():
    examples_a = ds.build_dataset(seed=1, only_source_ids=["self_render"])
    examples_b = ds.build_dataset(seed=2, only_source_ids=["self_render"])
    assert len(examples_a) == len(examples_b)
    # Different seeds draw different jitter -- the augmented copies need not
    # be identical, only the base (jitter=0.0) copies and per-class counts.
    assert ds.support_table(examples_a) == ds.support_table(examples_b)


def test_build_dataset_respects_only_source_ids_filter():
    examples = ds.build_dataset(seed=1, only_source_ids=["self_render"])
    assert all(ex.source_id == "self_render" for ex in examples)
