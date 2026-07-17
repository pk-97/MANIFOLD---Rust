"""D10 metric definition tests — these lock the frozen tolerances/semantics
in place. A change to any assertion here IS the Peter escalation D10 talks
about; don't "fix" a failing test by loosening a tolerance."""

from __future__ import annotations

from eval import metrics


def test_event_prf_perfect_match():
    truth = [1.0, 2.0, 3.0]
    pred = [1.01, 1.99, 3.02]
    prf = metrics.event_prf(pred, truth)
    assert prf.precision == 1.0
    assert prf.recall == 1.0
    assert prf.f1 == 1.0
    assert prf.true_positives == 3


def test_event_prf_outside_tolerance_is_a_miss():
    truth = [1.0]
    pred = [1.2]  # 200ms away, tolerance is 50ms
    prf = metrics.event_prf(pred, truth)
    assert prf.true_positives == 0
    assert prf.false_positives == 1
    assert prf.false_negatives == 1


def test_event_prf_empty_both_is_perfect():
    prf = metrics.event_prf([], [])
    assert prf.f1 == 1.0


def test_event_prf_false_positive_only():
    prf = metrics.event_prf([1.0], [])
    assert prf.precision == 0.0
    assert prf.false_positives == 1


def test_beat_prf_tolerance_is_70ms():
    truth = [0.0, 0.5, 1.0]
    pred = [0.069, 0.5, 1.069]  # just inside 70ms on the edges
    prf = metrics.beat_prf(pred, truth)
    assert prf.true_positives == 3


def test_beat_prf_just_outside_70ms():
    truth = [0.0]
    pred = [0.071]
    prf = metrics.beat_prf(pred, truth)
    assert prf.f1 == 0.0


def test_section_boundary_prf_converts_bar_tolerance_from_bpm():
    # 120 bpm, 4/4 -> 2s/bar -> 0.5 bar tolerance = 1.0s
    truth = [10.0]
    pred = [10.9]  # within 1.0s
    prf = metrics.section_boundary_prf(pred, truth, bpm=120.0)
    assert prf.true_positives == 1

    pred_far = [11.1]  # outside 1.0s
    prf_far = metrics.section_boundary_prf(pred_far, truth, bpm=120.0)
    assert prf_far.true_positives == 0


def test_section_label_accuracy_only_counts_matched_pairs():
    truth_b, truth_l = [10.0, 20.0], ["drop", "break"]
    pred_b, pred_l = [10.05, 50.0], ["drop", "intro"]  # second pred is unmatched (no truth nearby)
    acc = metrics.section_label_accuracy(pred_b, pred_l, truth_b, truth_l, tolerance_sec=0.5)
    assert acc == 1.0  # only the matched pair counts, and it's correct


def test_duration_iou_full_overlap_is_one():
    assert metrics.duration_iou((0.0, 2.0), (0.0, 2.0)) == 1.0


def test_duration_iou_no_overlap_is_zero():
    assert metrics.duration_iou((0.0, 1.0), (2.0, 3.0)) == 0.0


def test_duration_iou_partial():
    iou = metrics.duration_iou((0.0, 2.0), (1.0, 3.0))
    assert abs(iou - (1.0 / 3.0)) < 1e-9


def test_mean_duration_iou_matches_by_onset_then_averages():
    pred = [(0.0, 2.0), (10.0, 12.0)]
    truth = [(0.02, 2.0), (10.0, 11.0)]
    mean_iou, n_matched = metrics.mean_duration_iou(pred, truth)
    assert n_matched == 2
    assert 0.0 < mean_iou <= 1.0


def test_domain_aggregate_splits_electronic_from_other_and_reports_overall():
    rows = [
        {"domain": "electronic", "f1": 0.8},
        {"domain": "electronic", "f1": 0.6},
        {"domain": "other", "f1": 0.2},
        {"domain": None, "f1": 0.9},  # missing domain -> skipped everywhere
    ]
    agg = metrics.domain_aggregate(rows, "f1")
    assert agg["electronic"]["n"] == 2
    assert abs(agg["electronic"]["mean"] - 0.7) < 1e-9
    assert agg["other"]["n"] == 1
    assert agg["overall"]["n"] == 3  # the None-domain row is excluded from overall too


def test_density_diagnostics_is_advisory_shape_only():
    diag = metrics.density_diagnostics({"kick": [0.5, 1.5, 2.5, 3.5]}, track_duration_sec=8.0, bpm=120.0)
    # 8s @ 120bpm, 4/4 -> 4 bars; 4 events / 4 bars = 1/bar
    assert abs(diag.events_per_bar - 1.0) < 1e-6
