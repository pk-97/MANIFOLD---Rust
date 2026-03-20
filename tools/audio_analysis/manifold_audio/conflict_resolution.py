"""Post-classification metrics: event counting and overlap rate."""

from __future__ import annotations

from typing import Dict, Sequence

from manifold_audio.math_utils import _clamp, _safe_div
from manifold_audio.models import ScoredEvent


def _count_event_types(events: Sequence[ScoredEvent]) -> Dict[str, int]:
    out: Dict[str, int] = {}
    for event in events:
        out[event.type] = out.get(event.type, 0) + 1
    return out


def _overlap_rate(events: Sequence[ScoredEvent], type_a: str, type_b: str, window_sec: float) -> float:
    if window_sec <= 0.0:
        return 0.0

    times_a = [event.time for event in events if event.type == type_a]
    times_b = [event.time for event in events if event.type == type_b]
    if not times_a or not times_b:
        return 0.0

    j = 0
    overlaps = 0
    for t in times_a:
        while j < len(times_b) and times_b[j] < (t - window_sec):
            j += 1
        if j < len(times_b) and abs(times_b[j] - t) <= window_sec:
            overlaps += 1
    return _clamp(_safe_div(float(overlaps), float(max(1, len(times_a)))), 0.0, 1.0)
