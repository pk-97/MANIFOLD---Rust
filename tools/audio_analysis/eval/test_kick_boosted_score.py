import copy
import json
from pathlib import Path
import tempfile
import unittest

import numpy as np
from sklearn.ensemble import GradientBoostingClassifier

from .kick_boosted_score import FoldFitter, export_model, parameters, predict_score
from .kick_subspace_score import _training_data
from .run_kick_fusion_trial import fit_without as linear_fit


def record(name, seed=0):
    rng = np.random.default_rng(seed)
    x = rng.normal(size=(64, 4))
    labels = ((x[:, 0]*x[:, 1] + .2*x[:, 2]) > 0).astype(int)
    ids = np.where(labels == 1, np.cumsum(labels)//2, -1)
    return dict(track=name, features=x, labels=labels, event_ids=ids,
                training_mask=np.ones(len(x), dtype=bool))


class BoostedTests(unittest.TestCase):
    def test_exported_json_predictions_match_sklearn_for_every_configuration(self):
        r = record('a')
        for depth in (1, 2, 3):
            with self.subTest(depth=depth):
                _, x, y, w = _training_data([r], 'held')
                mean = np.sum(x*w[:, None], axis=0)
                scale = np.maximum(np.sqrt(np.sum((x-mean)**2*w[:, None], axis=0)), 1e-3)
                z = np.clip((x-mean)/scale, -8, 8)
                fitted = GradientBoostingClassifier(**parameters(depth)).fit(z, y, sample_weight=w)
                model = json.loads(json.dumps(export_model(fitted, mean, scale, ['a'])))
                # Includes both unseen rows and values immediately around tree splits.
                probes = np.random.default_rng(21).normal(size=(31, 4))
                for tree in model['trees']:
                    for feature, threshold in zip(tree['feature'], tree['threshold']):
                        if feature >= 0:
                            for offset in (-1e-7, 0, 1e-7):
                                point = mean.copy()
                                point[feature] += scale[feature]*(threshold+offset)
                                probes = np.vstack((probes, point))
                normalised = np.clip((probes-mean)/scale, -8, 8)
                np.testing.assert_allclose(predict_score(model, probes),
                    fitted.predict_proba(normalised)[:, 1], atol=1e-12, rtol=0)

    def test_prediction_is_row_local_and_uses_frozen_clipping(self):
        model = FoldFitter().fit([record('a')], 'held', 3)
        probes = record('probe', 18)['features']
        batched = predict_score(model, probes)
        individual = np.array([predict_score(model, [row])[0] for row in probes])
        np.testing.assert_array_equal(batched, individual)
        np.testing.assert_array_equal(batched[::-1], predict_score(model, probes[::-1]))
        extreme = np.array([[1e100, -1e100, 1e100, -1e100]])
        clipped = np.asarray(model['mean']) + np.asarray(model['scale'])*[[8, -8, 8, -8]]
        np.testing.assert_array_equal(predict_score(model, extreme), predict_score(model, clipped))
        self.assertEqual(predict_score(model, np.empty((0, 4))).shape, (0,))

    def test_held_out_perturbations_never_enter_fit_or_cache_key(self):
        records = [record('a'), record('b', 1), record('outer', 2), record('inner', 3)]
        fitter = FoldFitter()
        outer = fitter.fit(records, 'outer', 2)
        inner_records = [r for r in records if r['track'] != 'outer']
        inner = fitter.fit(inner_records, 'inner', 2)
        records[-2].update(features=np.array([np.nan]), labels=None,
                           event_ids=None, training_mask=None)
        self.assertEqual(outer, fitter.fit(records, 'outer', 2))
        records[-1].update(features=np.array([np.nan]), labels=None,
                           event_ids=None, training_mask=None)
        self.assertEqual(inner, fitter.fit(inner_records, 'inner', 2))
        self.assertEqual(inner['training_tracks'], ['a', 'b'])
        self.assertEqual(fitter.statistics()['fits'], 2)

    def test_weights_conserve_equal_songs_classes_and_positive_events(self):
        records = [record('a'), record('b', 5)]
        records[1]['training_mask'][:9] = False
        _, x, y, w = _training_data(records, 'held')
        offset = 0
        for r in records:
            selected = r['training_mask']
            labels, ids = r['labels'][selected], r['event_ids'][selected]
            weights = w[offset:offset+len(labels)]
            self.assertAlmostEqual(weights.sum(), .5)
            self.assertAlmostEqual(weights[labels == 0].sum(), .25)
            self.assertAlmostEqual(weights[labels == 1].sum(), .25)
            events = np.unique(ids[labels == 1])
            for event in events:
                self.assertAlmostEqual(weights[(labels == 1) & (ids == event)].sum(), .25/len(events))
            offset += len(labels)
        model = FoldFitter().fit(records, 'held', 1)
        linear = linear_fit(records, 'held')
        self.assertEqual(model['mean'], linear['mean'])
        self.assertEqual(model['scale'], linear['scale'])

    def test_identical_training_sets_reuse_hash_verified_disk_models(self):
        records = [record('a'), record('b', 1)]
        with tempfile.TemporaryDirectory() as directory:
            first = FoldFitter(directory)
            model = first.fit(records, 'held1', 2)
            reused = first.fit(records, 'held2', 2)
            self.assertEqual(first.statistics()['fits'], 1)
            model['held_out'] = 'held2'
            self.assertEqual(model, reused)
            second = FoldFitter(directory)
            self.assertEqual(reused, second.fit(records, 'held2', 2))
            self.assertEqual(second.statistics()['disk_hits'], 1)
            changed = copy.deepcopy(records)
            changed[0]['features'][0, 0] += 1
            second.fit(changed, 'held2', 2)
            self.assertEqual(second.statistics()['fits'], 1)
            path = next(Path(directory).glob('*.json'))
            stored = json.loads(path.read_text())
            stored['model']['initial_log_odds'] += 1
            path.write_text(json.dumps(stored))
            with self.assertRaisesRegex(ValueError, 'checksum'):
                FoldFitter(directory).fit(records, 'held2', 2)

    def test_invalid_inputs_fail_explicitly(self):
        for depth in (0, 4, True, 1., '2'):
            with self.subTest(depth=depth), self.assertRaises(ValueError):
                parameters(depth)
        fitter = FoldFitter()
        with self.assertRaisesRegex(ValueError, 'unique'):
            fitter.fit([record('a'), record('a')], 'held', 1)
        with self.assertRaisesRegex(ValueError, 'no training'):
            fitter.fit([record('held')], 'held', 1)
        model = fitter.fit([record('a')], 'held', 1)
        for features in ([[np.nan]*4], [[np.inf]*4], [[1]*3], [1]*4):
            with self.subTest(features=features), self.assertRaises(ValueError):
                predict_score(model, features)


if __name__ == '__main__':
    unittest.main()
