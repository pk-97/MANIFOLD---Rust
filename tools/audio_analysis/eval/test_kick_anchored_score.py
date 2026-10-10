import copy
import json
import unittest

import numpy as np
from scipy.special import expit

from .kick_anchored_score import FrozenFitter, TREE_WEIGHTS, predict_score
from .kick_boosted_score import training_key


def record(name, offset=0):
    return dict(track=name, features=np.asarray([[0., 1.], [2., -1.]])+offset,
        labels=np.asarray([0, 1]), event_ids=np.asarray([-1, 0]),
        training_mask=np.ones(2, bool))


def components(records, held):
    train = [r for r in records if r['track'] != held]
    common = dict(training_tracks=[r['track'] for r in train], held_out=held,
                  mean=[0., 0.], scale=[1., 1.])
    linear = dict(common, weights=[1., -.5], intercept=.2)
    tree = dict(common, training_input_keys=[list(training_key(r)) for r in train],
        max_depth=3, initial_log_odds=.1, learning_rate=.05,
        trees=[dict(left=[1, -1, -1], right=[2, -1, -1], feature=[0, -2, -2],
                    threshold=[1., -2., -2.], value=[0., -2., 3.])])
    return linear, tree


def variants(records):
    linear, tree = dict(tracks=[]), dict(tracks=[])
    for outer in records:
        a, b = components(records, outer['track'])
        ra, rb = dict(model=a, calibration=dict(inner_folds=[])), dict(model=b, calibration=dict(inner_folds=[]))
        inner = [r for r in records if r['track'] != outer['track']]
        for held in inner:
            ia, ib = components(inner, held['track'])
            ra['calibration']['inner_folds'].append(dict(model=ia))
            rb['calibration']['inner_folds'].append(dict(model=ib))
        linear['tracks'].append(ra); tree['tracks'].append(rb)
    return linear, tree


class AnchoredTests(unittest.TestCase):
    def setUp(self):
        self.records = [record('a'), record('b', 1), record('held', 2)]
        self.linear, self.tree = variants(self.records)

    def test_blend_algebra_export_and_row_local_prediction(self):
        bank = FrozenFitter(self.linear, self.tree)
        rows = np.asarray([[0., 1.], [2., -1.], [1e10, -1e10]])
        linear_logits = np.clip(rows, -8, 8) @ np.asarray([1., -.5])+.2
        tree_logits = np.asarray([.1-.05*2, .1+.05*3, .1+.05*3])
        for weight in TREE_WEIGHTS:
            model = json.loads(json.dumps(bank.fit(self.records, 'held', weight)))
            scores = predict_score(model, rows)
            np.testing.assert_allclose(scores, expit((1-weight)*linear_logits+weight*tree_logits), atol=1e-15)
            np.testing.assert_array_equal(scores, [predict_score(model, [x])[0] for x in rows])
            self.assertEqual(predict_score(model, np.empty((0, 2))).shape, (0,))

    def test_outer_and_inner_held_out_perturbation_never_enter_reuse(self):
        bank = FrozenFitter(self.linear, self.tree)
        outer = bank.fit(self.records, 'held', .5)
        inner_records = self.records[:2]
        inner = bank.fit(inner_records, 'b', .5)
        self.records[-1].update(features=None, labels=None, event_ids=None, training_mask=None)
        self.assertEqual(outer, bank.fit(self.records, 'held', .5))
        self.records[1].update(features=None, labels=None, event_ids=None, training_mask=None)
        self.assertEqual(inner, bank.fit(inner_records, 'b', .5))
        self.assertEqual(inner['training_tracks'], ['a'])

    def test_training_input_changes_and_mismatched_components_are_rejected(self):
        bank = FrozenFitter(self.linear, self.tree)
        changed = copy.deepcopy(self.records)
        changed[0]['features'][0, 0] += .1
        with self.assertRaisesRegex(ValueError, 'provenance'):
            bank.fit(changed, 'held', .5)
        with self.assertRaisesRegex(ValueError, 'ordered training-song set'):
            bank.fit(self.records[::-1], 'held', .5)
        with self.assertRaisesRegex(ValueError, 'frozen'):
            bank.fit(self.records, 'held', .6)
        mismatched = copy.deepcopy(self.tree)
        mismatched['tracks'][0]['model']['mean'][0] = 2
        with self.assertRaisesRegex(ValueError, 'normalisation'):
            FrozenFitter(self.linear, mismatched)
        leaked = copy.deepcopy(self.tree)
        leaked['tracks'][0]['model']['held_out'] = 'b'
        with self.assertRaisesRegex(ValueError, 'exclusion'):
            FrozenFitter(self.linear, leaked)

    def test_duplicate_training_sets_must_have_identical_predictions(self):
        changed = copy.deepcopy(self.linear)
        changed['tracks'][0]['calibration']['inner_folds'][0]['model']['intercept'] += 1
        with self.assertRaisesRegex(ValueError, 'different cached predictions'):
            FrozenFitter(changed, self.tree)


if __name__ == '__main__':
    unittest.main()
