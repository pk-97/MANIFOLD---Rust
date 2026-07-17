"""Parameter sweeps over cached bundles — dev-split only, by construction (D9).

D9's Goodhart guard: "the harness enforces it structurally (separate
directories, the sweep CLI physically cannot take" a path to the other
split. This module's public surface only accepts fixtures already tagged
for tuning (fixtures.toml split == "dev") — there is no parameter anywhere
in this file, in any function signature, CLI flag, or docstring, that names
or accepts the acceptance-only split. Acceptance scoring against that split
lives in run.py's separate `--set` flag, a different invocation entirely
(§3, forbidden item (c): "let the sweep 'just once' read [that split] to
check progress — no").

P1 ships the cache-safe skeleton; the actual grid/random search over
post-processing params (rolling-median window, threshold k, refractory
windows) is P4's deliverable (docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md P4).
"""

from __future__ import annotations

import itertools
from dataclasses import dataclass
from typing import Any, Callable, Dict, List, Sequence, Tuple

from eval.bundles import AnalysisBundle

# Fixture ids this module is allowed to touch. run.py builds this list by
# filtering fixtures.toml to split == "dev" and passes only those bundles in
# — sweep.py itself never reads fixtures.toml or resolves fixture ids, so
# there is no path in this file that could reach the other split even by
# accident.
DEV_ONLY_GUARD_NAME = "dev_bundles"


@dataclass(frozen=True)
class SweepResult:
    params: Dict[str, Any]
    score: float
    per_track: Dict[str, float]


def grid_sweep(
    dev_bundles: Sequence[AnalysisBundle],
    param_grid: Dict[str, Sequence[Any]],
    objective_fn: Callable[[AnalysisBundle, Dict[str, Any]], float],
) -> List[SweepResult]:
    """Exhaustive grid search over param_grid, scored by objective_fn against
    dev_bundles only (the parameter name is load-bearing, not decorative —
    see module docstring). objective_fn(bundle, params) -> a scalar score
    for that one track under those params; grid_sweep aggregates the mean
    across dev_bundles per parameter combination.

    Returns every combination's SweepResult, sorted best-first (caller picks
    the objective's sense — this just doesn't presume higher-is-better)."""
    keys = list(param_grid.keys())
    results: List[SweepResult] = []
    for combo in itertools.product(*(param_grid[k] for k in keys)):
        params = dict(zip(keys, combo))
        per_track: Dict[str, float] = {}
        for bundle in dev_bundles:
            per_track[bundle.content_hash] = objective_fn(bundle, params)
        score = sum(per_track.values()) / len(per_track) if per_track else 0.0
        results.append(SweepResult(params=params, score=score, per_track=per_track))
    return results


def best_result(results: Sequence[SweepResult], higher_is_better: bool = True) -> SweepResult:
    if not results:
        raise ValueError("no sweep results to select from")
    key = (lambda r: r.score) if higher_is_better else (lambda r: -r.score)
    return max(results, key=key)
