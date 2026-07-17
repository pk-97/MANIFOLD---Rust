"""DENSE_IN_WINDOW machinery (sweep_p4.derive_active_windows /
filter_to_windows) — pure-function tests, no audio needed."""

from eval.sweep_p4 import (
    ACTIVE_RUN_GAP_BEATS,
    ACTIVE_RUN_PAD_BEATS,
    derive_active_windows,
    filter_to_windows,
)

BPM = 120.0  # 0.5 s/beat -> gap 4.0s, pad 1.0s at the default knobs
SPB = 60.0 / BPM
GAP = ACTIVE_RUN_GAP_BEATS * SPB
PAD = ACTIVE_RUN_PAD_BEATS * SPB


def test_empty_and_zero_bpm():
    assert derive_active_windows([], BPM) == []
    assert derive_active_windows([1.0], 0.0) == []
    assert filter_to_windows([1.0], []) == []
    assert filter_to_windows([], [(0.0, 1.0)]) == []


def test_single_onset_gets_padded_window():
    (w,) = derive_active_windows([10.0], BPM)
    assert w == (10.0 - PAD, 10.0 + PAD)


def test_run_splits_only_past_gap():
    just_under = [0.0, GAP - 0.01]
    assert len(derive_active_windows(just_under, BPM)) == 1
    just_over = [0.0, GAP + 0.01]
    w = derive_active_windows(just_over, BPM)
    assert len(w) == 2
    assert w[0] == (0.0 - PAD, 0.0 + PAD)
    assert w[1] == (GAP + 0.01 - PAD, GAP + 0.01 + PAD)


def test_dense_run_is_one_window_spanning_first_to_last():
    ts = [10.0 + 0.5 * i for i in range(16)]  # 8s of 1-beat spacing
    (w,) = derive_active_windows(ts, BPM)
    assert w == (10.0 - PAD, ts[-1] + PAD)


def test_unsorted_input_is_sorted_first():
    assert derive_active_windows([5.0, 1.0], BPM) == derive_active_windows([1.0, 5.0], BPM)


def test_filter_keeps_inside_drops_outside():
    windows = [(1.0, 2.0), (10.0, 12.0)]
    times = [0.5, 1.0, 1.5, 2.0, 5.0, 10.0, 11.9, 12.0, 12.1]
    assert filter_to_windows(times, windows) == [1.0, 1.5, 2.0, 10.0, 11.9, 12.0]


def test_windowed_scoring_shape_matches_dense_semantics():
    """A prediction between runs (silence that proves nothing) must not
    count against precision; one inside a run must."""
    truth = [1.0, 1.5, 2.0, 30.0, 30.5]
    windows = derive_active_windows(truth, BPM)
    pred = [1.0, 1.5, 2.0, 15.0, 30.0, 30.5]  # 15.0 is between runs
    kept = filter_to_windows(pred, windows)
    assert 15.0 not in kept
    assert kept == [1.0, 1.5, 2.0, 30.0, 30.5]
