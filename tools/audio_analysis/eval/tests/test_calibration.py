"""eval/calibration.py tests (BUG-235 fix) — covers the pure-logic pieces
that don't require ADTOF model inference: the nearest-signed-diff matcher,
apply_calibration's arithmetic, and the calibration file round-trip. The
detector-scoring functions themselves (measure_kick_calibration_for_fixture,
score_kick_onset_fixture_calibrated) are exercised by the real measurement
run (eval/calibration/kick_onset_calibration.json + eval/scoreboard/
p4_calibrated_baseline.json), not here — same convention as
eval/tests/test_full_pack_baseline.py / test_beat_tracker_alignment.py."""

from __future__ import annotations

import json

from eval.calibration import (
    MANIFOLD_OWN_KICK_FIXTURE_IDS,
    _nearest_signed_diffs,
    apply_calibration,
    load_calibration,
    write_calibration_file,
)


def test_nearest_signed_diffs_finds_constant_early_bias():
    truth = [0.5, 1.5, 2.5, 3.5]
    pred = [t - 0.1 for t in truth]  # every prediction 100ms early
    diffs = _nearest_signed_diffs(pred, truth, window_sec=0.3)
    assert len(diffs) == 4
    assert all(abs(d - (-0.1)) < 1e-9 for d in diffs)


def test_nearest_signed_diffs_drops_matches_outside_window():
    truth = [1.0]
    pred = [1.5]  # 500ms away
    diffs = _nearest_signed_diffs(pred, truth, window_sec=0.3)
    assert diffs == []


def test_nearest_signed_diffs_empty_truth_is_empty():
    assert _nearest_signed_diffs([1.0, 2.0], [], window_sec=0.3) == []


def test_apply_calibration_shifts_early_predictions_later():
    # offset = pred - truth = -0.125 (early bias) -> corrected = pred - offset = pred + 0.125
    pred = [1.0, 2.0]
    corrected = apply_calibration(pred, offset_sec=-0.125)
    assert corrected == [1.125, 2.125]


def test_apply_calibration_zero_offset_is_identity():
    pred = [0.3, 0.7, 1.9]
    assert apply_calibration(pred, offset_sec=0.0) == pred


def test_write_and_load_calibration_round_trip(tmp_path):
    rows = [
        {"id": "apricots_128bpm", "median_offset_sec": -0.125, "n_pred": 14, "n_truth": 16,
         "n_matched_for_calibration": 14, "min_offset_sec": -0.13, "max_offset_sec": -0.12, "match_window_sec": 0.3},
        {"id": "bad_guy_128bpm", "median_offset_sec": -0.025, "n_pred": 23, "n_truth": 17,
         "n_matched_for_calibration": 16, "min_offset_sec": -0.05, "max_offset_sec": 0.0, "match_window_sec": 0.3},
    ]
    out_path = tmp_path / "kick_onset_calibration.json"
    written = write_calibration_file(rows, out_path)
    assert written == out_path

    loaded = load_calibration(out_path)
    assert loaded == {"apricots_128bpm": -0.125, "bad_guy_128bpm": -0.025}

    # File is honest about its own header/provenance (documented, not blank).
    payload = json.loads(out_path.read_text())
    assert "BUG-235" in payload["_comment"]
    assert "scoring seam" in payload["_comment"]
    assert payload["match_window_sec"] == 0.3


def test_manifold_own_kick_fixture_ids_are_the_five_from_the_design_doc():
    assert MANIFOLD_OWN_KICK_FIXTURE_IDS == [
        "apricots_128bpm",
        "bad_guy_128bpm",
        "feel_the_vibration_174bpm",
        "inhale_exhale_145bpm",
        "tears_140bpm",
    ]
