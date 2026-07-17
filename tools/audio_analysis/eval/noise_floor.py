"""D11 — noise floor measurement.

"Torch on MPS is not bit-deterministic; P1 measures per-metric rerun
variance (N=3 full model passes on the dev set) and stores it as the noise
floor." Change acceptance elsewhere in the harness (P4/P6) is delta > 2x
this noise floor, on held-out.

This module reruns the SAME detector on the SAME audio N times and reports
the spread per metric — it does not compare to any ground truth itself
(that's metrics.py's job, called N times by the caller).
"""

from __future__ import annotations

import json
import statistics
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Callable, Dict, List

N_RUNS_DEFAULT = 3


@dataclass(frozen=True)
class NoiseFloorStat:
    metric_name: str
    values: List[float]
    mean: float
    stdev: float
    range: float  # max - min, the more conservative "noise floor" bound

    @property
    def two_x_stdev(self) -> float:
        return 2.0 * self.stdev


def measure_noise_floor(
    metric_fn: Callable[[], Dict[str, float]],
    n_runs: int = N_RUNS_DEFAULT,
) -> Dict[str, NoiseFloorStat]:
    """metric_fn() reruns the detector-under-test and returns {metric_name:
    value} for one pass. Called n_runs times; per-metric stdev/range across
    those runs IS the noise floor (D11)."""
    runs: List[Dict[str, float]] = [metric_fn() for _ in range(n_runs)]
    if not runs:
        return {}
    metric_names = runs[0].keys()
    stats: Dict[str, NoiseFloorStat] = {}
    for name in metric_names:
        values = [r[name] for r in runs if name in r]
        if len(values) < 2:
            stats[name] = NoiseFloorStat(name, values, values[0] if values else 0.0, 0.0, 0.0)
            continue
        stats[name] = NoiseFloorStat(
            metric_name=name,
            values=values,
            mean=statistics.mean(values),
            stdev=statistics.stdev(values),
            range=max(values) - min(values),
        )
    return stats


def write_noise_floor_report(stats: Dict[str, NoiseFloorStat], path: Path, n_runs: int) -> None:
    payload = {
        "_comment": "D11 noise floor: N reruns of the SAME detector on the SAME dev audio. "
        "Acceptance elsewhere in the harness (P4/P6) requires delta > 2x stdev here.",
        "n_runs": n_runs,
        "metrics": {name: asdict(stat) for name, stat in stats.items()},
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2))
