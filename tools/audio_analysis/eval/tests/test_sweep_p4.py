"""eval/sweep_p4.py tests — round-2 correction: truth-type-aware scoring
(dense contributes full P/R/F1; sparse_visual contributes recall +
active-passage precision only, never raw precision/F1 into the same
aggregate as dense). Pure-logic + synthetic-activation tests only — no
ADTOF/basic_pitch model inference (same convention as
test_precision_postprocessing.py / test_full_pack_baseline.py). Corpus
builders (_build_babyslakh_tracks etc., which need real fixture audio) are
exercised by the real sweep run's committed scoreboards, not here."""

from __future__ import annotations

import numpy as np
import pytest

from eval.sweep_p4 import (
    DENSE,
    SPARSE_VISUAL,
    TrackData,
    _is_dead_knob,
    _isolated_truth,
    active_passage_filter,
    score_config_for_class,
)
from manifold_audio.precision_postprocessing import BeatGridRef, PrecisionConfig


def _make_activation(n_frames: int, kick_peak_frames) -> np.ndarray:
    act = np.full((n_frames, 5), 0.02, dtype=np.float32)
    for f in kick_peak_frames:
        act[f, 0] = 0.9  # column 0 = kick
    return act


def _dense_track() -> TrackData:
    n = 300
    activations = _make_activation(n, [50])  # frame 50 @ fps=100 -> 0.50s
    return TrackData(
        id="dense_track", domain="electronic", source="babyslakh",
        audio=np.zeros(44100, dtype=np.float32), sr=44100, audio_path="dense.wav",
        activations=activations, fps=100, basic_pitch_notes=None,
        truth_type=DENSE, truth_by_class={"kick": [0.50]},
    )


def _sparse_track(extra_far_pred: bool) -> TrackData:
    """One real, labeled kick at 5.0s, plus (if extra_far_pred) an
    additional real (but unlabeled -- as sparse-visual truth always is)
    kick at 20.0s, far outside the +/-1s active-passage window at 120bpm
    (2 beats = 1.0s)."""
    n = 3000
    peak_frames = [500]  # 5.00s
    if extra_far_pred:
        peak_frames.append(2000)  # 20.00s
    activations = _make_activation(n, peak_frames)
    return TrackData(
        id="sparse_track", domain="electronic", source="liveshow",
        audio=np.zeros(44100, dtype=np.float32), sr=44100, audio_path="sparse.wav",
        activations=activations, fps=100, basic_pitch_notes=None,
        truth_type=SPARSE_VISUAL, truth_by_class={"kick": [5.00]},
        grid=BeatGridRef(bpm=120.0, anchor_sec=0.0),
    )


def test_active_passage_filter_keeps_near_drops_far():
    truth = [5.0]
    pred = [5.02, 20.0]  # one near (20ms away), one 15s away
    kept = active_passage_filter(pred, truth, window_sec=1.0)
    assert kept == [5.02]


def test_active_passage_filter_empty_inputs():
    assert active_passage_filter([], [1.0], window_sec=1.0) == []
    assert active_passage_filter([1.0], [], window_sec=1.0) == []


def test_isolated_truth_finds_only_the_lone_hit():
    # Two hits close together (< 2.0s isolation), one far away.
    times = [1.0, 1.5, 10.0]
    isolated = _isolated_truth(times, isolation_sec=2.0)
    assert isolated == [10.0]


def test_isolated_truth_single_event_is_isolated():
    assert _isolated_truth([5.0], isolation_sec=2.0) == [5.0]


def test_dense_track_contributes_full_prf_not_recall_only():
    corpus = [_dense_track()]
    config = PrecisionConfig()
    result = score_config_for_class(corpus, "kick", config)

    assert result["sparse_visual"]["n_tracks_scored"] == 0
    assert result["dense"]["n_tracks_scored"] == 1
    row = result["dense"]["per_track"][0]
    assert "precision" in row and "recall" in row and "kick_f1" in row
    assert row["kick_f1"] == pytest.approx(1.0)
    assert result["headline"]["dense_f1"] == pytest.approx(1.0)
    assert result["headline"]["sparse_recall"] is None


def test_sparse_track_contributes_recall_only_never_raw_precision():
    corpus = [_sparse_track(extra_far_pred=True)]
    config = PrecisionConfig()
    result = score_config_for_class(corpus, "kick", config)

    assert result["dense"]["n_tracks_scored"] == 0
    assert result["sparse_visual"]["n_tracks_scored"] == 1
    row = result["sparse_visual"]["per_track"][0]
    # Structural guarantee: a sparse-visual row NEVER carries the dense
    # per-track keys (no raw "precision", no "{class}_f1") -- only recall
    # and the separately-reported active-passage precision.
    assert "precision" not in row
    assert "kick_f1" not in row
    assert "recall" in row
    assert "active_passage_precision" in row
    assert result["headline"]["dense_f1"] is None
    assert result["headline"]["sparse_recall"] == pytest.approx(1.0)  # the one labeled kick WAS found


def test_sparse_active_passage_precision_excludes_far_unlabeled_hit():
    # With the extra far (20s) prediction, RAW precision would be 1/2 = 0.5
    # (a "false positive" that is actually just a real, unlabeled event).
    # Active-passage precision restricts to predictions within the window
    # and should read 1.0 (the only in-window prediction matches truth).
    corpus = [_sparse_track(extra_far_pred=True)]
    config = PrecisionConfig()
    result = score_config_for_class(corpus, "kick", config)
    row = result["sparse_visual"]["per_track"][0]
    assert row["n_pred"] == 2
    assert row["active_passage_precision"] == pytest.approx(1.0)
    assert row["n_pred_in_active_window"] == 1


def test_sparse_active_passage_precision_without_far_hit_is_also_one():
    corpus = [_sparse_track(extra_far_pred=False)]
    config = PrecisionConfig()
    result = score_config_for_class(corpus, "kick", config)
    row = result["sparse_visual"]["per_track"][0]
    assert row["n_pred"] == 1
    assert row["active_passage_precision"] == pytest.approx(1.0)


def test_mixed_corpus_dense_and_sparse_scored_separately():
    corpus = [_dense_track(), _sparse_track(extra_far_pred=True)]
    config = PrecisionConfig()
    result = score_config_for_class(corpus, "kick", config)
    assert result["dense"]["n_tracks_scored"] == 1
    assert result["sparse_visual"]["n_tracks_scored"] == 1
    # Dense F1 unaffected by the sparse track's extra "false positive".
    assert result["headline"]["dense_f1"] == pytest.approx(1.0)
    assert result["headline"]["sparse_recall"] == pytest.approx(1.0)


def _row(dense_f1, sparse_recall):
    return {
        "dense": {"mean_f1": dense_f1},
        "sparse_visual": {"mean_recall": sparse_recall},
    }


def test_is_dead_knob_true_when_neither_series_varies():
    rows = [_row(0.5, 0.3), _row(0.5, 0.3), _row(0.5, 0.3)]
    assert _is_dead_knob(rows) is True


def test_is_dead_knob_false_when_dense_varies():
    rows = [_row(0.5, 0.3), _row(0.6, 0.3), _row(0.4, 0.3)]
    assert _is_dead_knob(rows) is False


def test_is_dead_knob_false_when_only_sparse_varies():
    rows = [_row(0.5, 0.3), _row(0.5, 0.5), _row(0.5, 0.1)]
    assert _is_dead_knob(rows) is False


def test_is_dead_knob_true_with_all_none():
    rows = [_row(None, None), _row(None, None)]
    assert _is_dead_knob(rows) is True
