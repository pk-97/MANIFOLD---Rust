"""D10 metric definitions — FROZEN at P1.

docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md D10: "Every metric change after P1 is a
Peter escalation, because it invalidates all recorded baselines." Do not add,
remove, or redefine a metric here without that escalation — extend with a new
function under a new name instead of changing an existing one's semantics.

Tolerances (frozen, D10):
    EVENT_TOLERANCE_SEC   = 0.050  per-instrument event P/R/F1
    BEAT_TOLERANCE_SEC    = 0.070  beat F1 (standard MIR tolerance)
    DOWNBEAT_TOLERANCE_SEC= 0.070  downbeat F1
    SECTION_TOLERANCE_BAR = 0.5    section boundary F1 (converted to seconds
                                   by the caller via the track's beat grid)

Uses mir_eval where its definitions match D10 (onset F1, beat F1) — per §3:
"mir_eval where it matches the definition." Section-boundary and duration-IoU
have no drop-in mir_eval equivalent shaped like D10 wants, so they are
hand-rolled here (greedy nearest-match within tolerance, one-to-one).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Dict, List, Optional, Sequence, Tuple

import numpy as np

try:
    import mir_eval
except ImportError:  # pragma: no cover - exercised only if runtime is misconfigured
    mir_eval = None  # type: ignore[assignment]

EVENT_TOLERANCE_SEC = 0.050
BEAT_TOLERANCE_SEC = 0.070
DOWNBEAT_TOLERANCE_SEC = 0.070
SECTION_TOLERANCE_BAR = 0.5


@dataclass(frozen=True)
class PRF:
    """Precision/recall/F1 plus the raw counts that produced them."""

    precision: float
    recall: float
    f1: float
    true_positives: int
    false_positives: int
    false_negatives: int

    def to_dict(self) -> Dict[str, float]:
        return {
            "precision": self.precision,
            "recall": self.recall,
            "f1": self.f1,
            "tp": self.true_positives,
            "fp": self.false_positives,
            "fn": self.false_negatives,
        }


def _require_mir_eval() -> None:
    if mir_eval is None:
        raise RuntimeError(
            "mir_eval is not importable in this runtime. It is a required "
            "eval-harness dependency (requirements.runtime.mac.txt) — "
            "re-stage the bundled runtime."
        )


def _greedy_match_count(
    pred: Sequence[float], truth: Sequence[float], tolerance_sec: float
) -> Tuple[int, int, int]:
    """One-to-one greedy nearest-match within tolerance. Returns (tp, fp, fn).

    Used for metrics mir_eval has no matching primitive for (section
    boundaries, duration-anchored matching). Sorted-merge greedy match:
    each truth event may be claimed by at most one prediction and vice versa.
    """
    pred_sorted = sorted(pred)
    truth_sorted = sorted(truth)
    used_truth = [False] * len(truth_sorted)
    tp = 0
    for p in pred_sorted:
        best_idx: Optional[int] = None
        best_dist = tolerance_sec
        for i, t in enumerate(truth_sorted):
            if used_truth[i]:
                continue
            dist = abs(p - t)
            if dist <= best_dist:
                best_dist = dist
                best_idx = i
        if best_idx is not None:
            used_truth[best_idx] = True
            tp += 1
    fp = len(pred_sorted) - tp
    fn = len(truth_sorted) - tp
    return tp, fp, fn


def _prf_from_counts(tp: int, fp: int, fn: int) -> PRF:
    precision = tp / (tp + fp) if (tp + fp) > 0 else (1.0 if fn == 0 else 0.0)
    recall = tp / (tp + fn) if (tp + fn) > 0 else (1.0 if fp == 0 else 0.0)
    f1 = 2 * precision * recall / (precision + recall) if (precision + recall) > 0 else 0.0
    return PRF(precision, recall, f1, tp, fp, fn)


def event_prf(
    pred_times_sec: Sequence[float],
    truth_times_sec: Sequence[float],
    tolerance_sec: float = EVENT_TOLERANCE_SEC,
) -> PRF:
    """Per-instrument event P/R/F1 at the frozen tolerance (default ±50ms, D10).

    Caller pre-filters both sequences to one instrument class. Uses
    mir_eval.onset.f_measure (its definition — greedy one-to-one nearest
    match within a symmetric window — matches D10's intent exactly).
    """
    _require_mir_eval()
    pred = np.asarray(sorted(pred_times_sec), dtype=np.float64)
    truth = np.asarray(sorted(truth_times_sec), dtype=np.float64)
    if len(truth) == 0 and len(pred) == 0:
        return PRF(1.0, 1.0, 1.0, 0, 0, 0)
    f1, precision, recall = mir_eval.onset.f_measure(truth, pred, window=tolerance_sec)
    tp, fp, fn = _greedy_match_count(pred.tolist(), truth.tolist(), tolerance_sec)
    return PRF(float(precision), float(recall), float(f1), tp, fp, fn)


def beat_prf(
    pred_beats_sec: Sequence[float],
    truth_beats_sec: Sequence[float],
    tolerance_sec: float = BEAT_TOLERANCE_SEC,
) -> PRF:
    """Beat F1 at ±70ms (D10, standard MIR tolerance)."""
    _require_mir_eval()
    pred = np.asarray(sorted(pred_beats_sec), dtype=np.float64)
    truth = np.asarray(sorted(truth_beats_sec), dtype=np.float64)
    if len(truth) == 0 and len(pred) == 0:
        return PRF(1.0, 1.0, 1.0, 0, 0, 0)
    f1 = float(mir_eval.beat.f_measure(truth, pred, f_measure_threshold=tolerance_sec))
    tp, fp, fn = _greedy_match_count(pred.tolist(), truth.tolist(), tolerance_sec)
    precision = tp / (tp + fp) if (tp + fp) > 0 else 0.0
    recall = tp / (tp + fn) if (tp + fn) > 0 else 0.0
    return PRF(precision, recall, f1, tp, fp, fn)


def downbeat_prf(
    pred_downbeats_sec: Sequence[float],
    truth_downbeats_sec: Sequence[float],
    tolerance_sec: float = DOWNBEAT_TOLERANCE_SEC,
) -> PRF:
    """Downbeat F1 at ±70ms (D10). Same shape as beat_prf; separate function
    because P2's gate tracks beat F1 and downbeat F1 as two distinct numbers
    (downbeat F1 is the one expected to *rise* with the Beat This swap)."""
    return beat_prf(pred_downbeats_sec, truth_downbeats_sec, tolerance_sec)


def section_boundary_prf(
    pred_boundaries_sec: Sequence[float],
    truth_boundaries_sec: Sequence[float],
    bpm: float,
    tolerance_bar: float = SECTION_TOLERANCE_BAR,
    beats_per_bar: float = 4.0,
) -> PRF:
    """Section boundary F1 at ±0.5 bar (D10). Bar tolerance is converted to
    seconds using the track's BPM (assumed locally constant across the
    tolerance window — fine at ±0.5 bar; a full tempo-map-aware version would
    integrate beats, not needed for the granularity D10 asks for)."""
    if bpm <= 0:
        raise ValueError("bpm must be > 0 to convert bar tolerance to seconds")
    sec_per_beat = 60.0 / bpm
    tolerance_sec = tolerance_bar * beats_per_bar * sec_per_beat
    tp, fp, fn = _greedy_match_count(
        list(pred_boundaries_sec), list(truth_boundaries_sec), tolerance_sec
    )
    return _prf_from_counts(tp, fp, fn)


def section_label_accuracy(
    pred_boundaries_sec: Sequence[float],
    pred_labels: Sequence[str],
    truth_boundaries_sec: Sequence[float],
    truth_labels: Sequence[str],
    tolerance_sec: float,
) -> float:
    """Label accuracy among boundary-matched pairs only (unmatched boundaries
    don't count toward or against label accuracy — that's what boundary F1
    already scores)."""
    assert len(pred_boundaries_sec) == len(pred_labels)
    assert len(truth_boundaries_sec) == len(truth_labels)
    truth_pairs = sorted(zip(truth_boundaries_sec, truth_labels), key=lambda p: p[0])
    used = [False] * len(truth_pairs)
    correct = 0
    matched = 0
    for p_time, p_label in sorted(zip(pred_boundaries_sec, pred_labels), key=lambda p: p[0]):
        best_idx: Optional[int] = None
        best_dist = tolerance_sec
        for i, (t_time, _t_label) in enumerate(truth_pairs):
            if used[i]:
                continue
            dist = abs(p_time - t_time)
            if dist <= best_dist:
                best_dist = dist
                best_idx = i
        if best_idx is not None:
            used[best_idx] = True
            matched += 1
            if p_label == truth_pairs[best_idx][1]:
                correct += 1
    return correct / matched if matched > 0 else 0.0


def duration_iou(pred_span: Tuple[float, float], truth_span: Tuple[float, float]) -> float:
    """Intersection-over-union of two (start_sec, end_sec) spans."""
    p0, p1 = pred_span
    t0, t1 = truth_span
    inter = max(0.0, min(p1, t1) - max(p0, t0))
    union = max(p1, t1) - min(p0, t0)
    if union <= 0:
        return 0.0
    return inter / union


def mean_duration_iou(
    pred_spans: Sequence[Tuple[float, float]],
    truth_spans: Sequence[Tuple[float, float]],
    onset_tolerance_sec: float = EVENT_TOLERANCE_SEC,
) -> Tuple[float, int]:
    """Match predicted sustained-event spans to truth spans by onset proximity
    (±onset_tolerance_sec, greedy one-to-one), then average IoU over matched
    pairs. Returns (mean_iou, n_matched); unmatched spans (misses/false
    positives) are already penalized by the corresponding event_prf call —
    this metric is duration quality *given* a matched onset, per D10."""
    pred_sorted = sorted(pred_spans, key=lambda s: s[0])
    truth_sorted = sorted(truth_spans, key=lambda s: s[0])
    used = [False] * len(truth_sorted)
    ious: List[float] = []
    for p in pred_sorted:
        best_idx: Optional[int] = None
        best_dist = onset_tolerance_sec
        for i, t in enumerate(truth_sorted):
            if used[i]:
                continue
            dist = abs(p[0] - t[0])
            if dist <= best_dist:
                best_dist = dist
                best_idx = i
        if best_idx is not None:
            used[best_idx] = True
            ious.append(duration_iou(p, truth_sorted[best_idx]))
    mean_iou = float(np.mean(ious)) if ious else 0.0
    return mean_iou, len(ious)


def domain_aggregate(
    rows: Sequence[Dict[str, object]], value_key: str, domain_key: str = "domain"
) -> Dict[str, Dict[str, float]]:
    """Per-domain mean of value_key, plus an 'overall' bucket (Peter,
    2026-07-17: the target is electronic/EDM; public MIR packs skew
    rock/pop/acoustic and must not dominate scoring). This function only
    EXPOSES the split — it does not decide tuning policy (which domain is
    optimized vs sanity-checked is the orchestrator's call, applied outside
    this module, never hardcoded into sweep.py).

    rows: e.g. scoreboard per-fixture result dicts, each carrying a
    `domain` field and the metric under value_key. Rows missing either key
    are skipped (reported via the 'n' count staying lower than len(rows))."""
    by_domain: Dict[str, List[float]] = {}
    all_values: List[float] = []
    for row in rows:
        domain = row.get(domain_key)
        value = row.get(value_key)
        if domain is None or value is None:
            continue
        by_domain.setdefault(str(domain), []).append(float(value))
        all_values.append(float(value))

    out: Dict[str, Dict[str, float]] = {}
    for domain, values in by_domain.items():
        out[domain] = {"mean": sum(values) / len(values), "n": len(values)}
    out["overall"] = {"mean": sum(all_values) / len(all_values) if all_values else 0.0, "n": len(all_values)}
    return out


@dataclass(frozen=True)
class DensityDiagnostics:
    """Clip-economy diagnostics — ADVISORY ONLY (D10). Never optimized: a
    sweep or acceptance gate that improves these at the cost of P/R/F1 is a
    Goodhart violation of D3 (clip-per-event is the contract, not density)."""

    events_per_bar: float
    events_per_lane_per_bar: Dict[str, float] = field(default_factory=dict)


def density_diagnostics(
    events_by_lane: Dict[str, Sequence[float]], track_duration_sec: float, bpm: float, beats_per_bar: float = 4.0
) -> DensityDiagnostics:
    if bpm <= 0 or track_duration_sec <= 0:
        return DensityDiagnostics(events_per_bar=0.0, events_per_lane_per_bar={})
    bar_sec = beats_per_bar * (60.0 / bpm)
    n_bars = track_duration_sec / bar_sec
    if n_bars <= 0:
        return DensityDiagnostics(events_per_bar=0.0, events_per_lane_per_bar={})
    total_events = sum(len(v) for v in events_by_lane.values())
    per_lane = {lane: len(times) / n_bars for lane, times in events_by_lane.items()}
    return DensityDiagnostics(events_per_bar=total_events / n_bars, events_per_lane_per_bar=per_lane)
