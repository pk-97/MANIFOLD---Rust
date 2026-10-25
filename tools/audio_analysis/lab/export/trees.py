"""Model file part `trees.*`: the event-measure forest.

A forest is flat node arrays: value, threshold (f64), feature, left, right (i32, node index inside its own tree),
missing_left, is_leaf (i32 0/1), offsets (i64, n_trees + 1: tree t owns nodes offsets[t]..offsets[t+1]), baseline
(f64 scalar), n_features (i64 scalar). Inference is sklearn's _predict_one_from_raw_data: NaN follows
missing_go_to_left, x <= threshold goes left; raw = baseline + sum of leaf values in tree order; probability =
expit(raw). The stage part (lab/export/stage.py) writes its forest the same way.
"""
from __future__ import annotations

import numpy as np

from tools.audio_analysis import kick_container

PREFIX = 'trees.'
FOREST = ('value', 'threshold', 'feature', 'left', 'right', 'missing_left', 'is_leaf', 'offsets', 'baseline',
          'n_features')
NAMES = frozenset(PREFIX + n for n in (*FOREST, 'measures', 'widths'))


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


def entries(record: dict) -> dict[str, np.ndarray]:
    from tools.audio_analysis.lab.release import parts
    t = record['trees']
    widths = [parts()['measures'][m] for m in t['measures']]
    return {**forest(t['model'], PREFIX, sum(widths)),
            'trees.measures': kick_container.text(','.join(t['measures'])),
            'trees.widths': np.array(widths, np.int64)}
