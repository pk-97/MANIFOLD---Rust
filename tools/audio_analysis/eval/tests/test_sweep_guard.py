"""D9's structural Goodhart guard: sweep.py's public surface only accepts
dev-tagged bundles by construction. This test is partly a tripwire — if
someone later adds a heldout-shaped parameter to grid_sweep/best_result,
these assertions (and the negative gate `rg 'heldout' eval/sweep.py`) should
both catch it."""

from __future__ import annotations

import inspect

from eval import sweep


def test_grid_sweep_signature_names_its_only_bundle_param_dev():
    sig = inspect.signature(sweep.grid_sweep)
    params = list(sig.parameters.keys())
    assert params[0] == sweep.DEV_ONLY_GUARD_NAME


def test_sweep_module_never_mentions_the_other_split_by_name():
    import pathlib

    source = pathlib.Path(sweep.__file__).read_text()
    assert "heldout" not in source.lower()


def test_grid_sweep_runs_a_trivial_grid():
    from eval.bundles import AnalysisBundle, BundleStamp

    stamp = BundleStamp(pipeline_version="test")
    bundles = [AnalysisBundle(content_hash=f"h{i}", stamp=stamp, scalar={"value": i}) for i in range(3)]

    def objective(bundle, params):
        return bundle.scalar["value"] * params["k"]

    results = sweep.grid_sweep(bundles, {"k": [1.0, 2.0]}, objective)
    assert len(results) == 2
    best = sweep.best_result(results)
    assert best.params["k"] == 2.0  # higher k -> higher mean score, is the best under higher_is_better
