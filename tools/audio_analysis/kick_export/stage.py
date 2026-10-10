"""Export hook for the trees, the blend, the 8 s stage and the fire rule (Rust: crates/manifold-audio/src/kick/{trees,stage}.rs).

Two forests, each as flat node arrays: 'trees.' (final['trees'], 69 features) and 'stage.' (final['stage'], 5 inputs).
Per forest: value, threshold (f64), feature, left, right (i32, node index inside its own tree), missing_left,
is_leaf (i32 0/1), offsets (i64, n_trees + 1: tree t owns nodes offsets[t]..offsets[t+1]), baseline (f64 scalar),
n_features (i64 scalar). Inference is sklearn's _predict_one_from_raw_data: NaN follows missing_go_to_left,
x <= threshold goes left; raw = baseline + sum of leaf values in tree order; probability = expit(raw).
"""
from __future__ import annotations

import sys
from pathlib import Path

import numpy as np

try:
    from tools.audio_analysis.eval.kick_goal_selfsim import POWER
except ImportError:
    sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
    from tools.audio_analysis.eval.kick_goal_selfsim import POWER

# The recipe's stream grid (48 kHz mono, 256-sample hop): emission times and the refractory are hop counts.
SAMPLE_RATE = 48000
HOP = 256
# run_kick_goal_fast.lg clips at 1e-6 before the blend's logit; kick_goal_selfsim.self_features clips lp at 1e-6.
BLEND_CLIP = 1e-6
LP_CLIP = 1e-6
# Blend = mean logit of trees and net (run_kick_goal_fast 'blend'): weights for (trees, net).
BLEND_WEIGHTS = (0.5, 0.5)


def forest(model, prefix, n_features):
    """Flat node arrays of a fitted binary HistGradientBoostingClassifier on numeric features."""
    assert len(model.classes_) == 2 and model.n_trees_per_iteration_ == 1, f'{prefix}: not a binary model'
    assert getattr(model, 'is_categorical_', None) is None, f'{prefix}: categorical features are not ported'
    assert model.n_features_in_ == n_features, f'{prefix}: {model.n_features_in_} features, want {n_features}'
    nodes = [it[0].nodes for it in model._predictors]
    for n in nodes:
        assert not n['is_categorical'].any(), f'{prefix}: categorical split'
        inner = np.flatnonzero(n['is_leaf'] == 0)
        idx = np.arange(len(n))
        # Children always follow their parent (grower order): Rust relies on it to prove traversal terminates.
        assert np.all(n['left'][inner] > idx[inner]) and np.all(n['right'][inner] > idx[inner])
        assert np.all(n['left'][inner] < len(n)) and np.all(n['right'][inner] < len(n))
    flat = np.concatenate(nodes)
    base = np.asarray(model._baseline_prediction, dtype=np.float64).reshape(())
    return {
        prefix + 'value': flat['value'].astype(np.float64),
        prefix + 'threshold': flat['num_threshold'].astype(np.float64),
        prefix + 'feature': flat['feature_idx'].astype(np.int32),
        prefix + 'left': flat['left'].astype(np.int32),
        prefix + 'right': flat['right'].astype(np.int32),
        prefix + 'missing_left': flat['missing_go_to_left'].astype(np.int32),
        prefix + 'is_leaf': flat['is_leaf'].astype(np.int32),
        prefix + 'offsets': np.cumsum([0] + [len(n) for n in nodes]).astype(np.int64),
        prefix + 'baseline': base,
        prefix + 'n_features': np.asarray(n_features, dtype=np.int64),
    }


def model_entries(final: dict) -> dict[str, np.ndarray]:
    out = {**forest(final['trees'], 'trees.', 69), **forest(final['stage'], 'stage.', 5)}
    out.update({
        'stage.cutoff': np.asarray(float(final['cutoff'])),
        'stage.refractory_s': np.asarray(float(final['refractory_s'])),
        'stage.window_s': np.asarray(float(final['window_s'])),
        'stage.power': np.asarray(float(POWER)),
        'stage.blend_clip': np.asarray(BLEND_CLIP),
        'stage.lp_clip': np.asarray(LP_CLIP),
        'stage.blend_weights': np.asarray(BLEND_WEIGHTS, dtype=np.float64),
        'stage.sample_rate': np.asarray(SAMPLE_RATE, dtype=np.int64),
        'stage.hop': np.asarray(HOP, dtype=np.int64),
    })
    return out
