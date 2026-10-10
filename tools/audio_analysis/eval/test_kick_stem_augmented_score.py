"""Weight conservation, source exclusion and unchanged inference for H4."""
import copy
import json
import tempfile
import unittest

import numpy as np
from sklearn.ensemble import GradientBoostingClassifier

from .kick_boosted_score import FoldFitter as OriginalFitter, parameters, _standardise
from .kick_stem_augmented_score import FoldFitter, augmented_training_data, predict_score
from .kick_subspace_score import _training_data
from .test_kick_boosted_score import record


def augmented(seed=0):
    rng = np.random.default_rng(seed)
    return dict(features=rng.normal(size=(20, 4)), labels=np.tile([1, 1, 1, 0, 0], 4),
                context_ids=np.repeat(['c1', 'c2', 'c3', 'c4'], 5), gains=np.tile([0, 1, 2, 1, 2], 4))


class StemAugmentedTests(unittest.TestCase):
    def test_song_class_event_and_context_mass(self):
        records = [record('a'), record('b', 1), record('c', 2)]
        groups = {'a': augmented(), 'c': augmented(3)}
        for mass in (.1, .25, .5):
            d = augmented_training_data(records, 'held', groups, mass)
            self.assertAlmostEqual(d['w'].sum(), 1.)
            self.assertAlmostEqual(d['w'][d['y'] == 1].sum(), .5)
            for r in records:
                family = d['families'] == r['track']
                natural = family & (d['kinds'] == 'natural')
                synthetic = family & (d['kinds'] == 'augmented')
                expected = mass if r['track'] in groups else 0.
                self.assertAlmostEqual(d['w'][family].sum(), 1/3)
                self.assertAlmostEqual(d['w'][natural].sum(), (1-expected)/3)
                self.assertAlmostEqual(d['w'][synthetic].sum(), expected/3)
                for label in (0, 1):
                    self.assertAlmostEqual(d['w'][family & (d['y'] == label)].sum(), 1/6)
                labels = r['labels'][r['training_mask']]; ids = r['event_ids'][r['training_mask']]
                events = np.unique(ids[labels == 1]); w = d['w'][natural]
                for event in events:
                    self.assertAlmostEqual(w[(ids == event) & (labels == 1)].sum(), (1-expected)/6/len(events))
                if expected:
                    for context in ('c1', 'c2', 'c3', 'c4'):
                        for label in (0, 1):
                            self.assertAlmostEqual(d['w'][synthetic & (d['context_ids'] == context) & (d['y'] == label)].sum(), expected/24)

    def test_original_normalisation_and_zero_mass_equivalence(self):
        records = [record('a'), record('b', 1)]
        groups = {'a': augmented()}
        _, x, y, w = _training_data(records, 'held')
        original = OriginalFitter().fit(records, 'held', 3)
        zero = FoldFitter(groups).fit(records, 'held', 0.)
        np.testing.assert_array_equal(predict_score(original, x), predict_score(zero, x))
        self.assertEqual(original['trees'], zero['trees'])
        for mass in (.1, .25, .5):
            d = augmented_training_data(records, 'held', groups, mass)
            np.testing.assert_array_equal(d['mean'], original['mean'])
            np.testing.assert_array_equal(d['scale'], original['scale'])
        d = augmented_training_data(records, 'held', groups, 0.)
        np.testing.assert_array_equal(d['x'], x); np.testing.assert_array_equal(d['y'], y)
        np.testing.assert_array_equal(d['w'], w)

    def test_entire_outer_and_inner_families_excluded_before_access_or_hash(self):
        records = [record('a'), record('b', 1), record('outer', 2), record('inner', 3)]
        groups = {r['track']: augmented(i) for i, r in enumerate(records)}
        fitter = FoldFitter(groups)
        outer = fitter.fit(records, 'outer', .25)
        inner_records = [r for r in records if r['track'] != 'outer']
        inner = fitter.fit(inner_records, 'inner', .25)
        groups['outer'] = dict(features=np.array([np.nan])); records[2]['features'] = np.array([np.nan])
        self.assertEqual(outer, fitter.fit(records, 'outer', .25))
        groups['inner'] = dict(features=np.array([np.nan])); records[3]['features'] = np.array([np.nan])
        self.assertEqual(inner, fitter.fit(inner_records, 'inner', .25))
        self.assertEqual(inner['augmentation_families'], ['a', 'b'])
        self.assertEqual(fitter.fits, 2)

    def test_export_parity_and_row_local_inference(self):
        records, groups = [record('a'), record('b', 1)], {'a': augmented()}
        d = augmented_training_data(records, 'held', groups, .25)
        z = _standardise(d['mean'], d['scale'], d['x'])
        clf = GradientBoostingClassifier(**parameters(3)).fit(z, d['y'], sample_weight=d['w'])
        model = json.loads(json.dumps(FoldFitter(groups).fit(records, 'held', .25)))
        probes = np.random.default_rng(14).normal(size=(35, 4))
        scores = predict_score(model, probes)
        np.testing.assert_allclose(scores, clf.predict_proba(_standardise(d['mean'], d['scale'], probes))[:, 1], atol=1e-12, rtol=0)
        np.testing.assert_array_equal(scores, [predict_score(model, [row])[0] for row in probes])
        np.testing.assert_array_equal(scores[::-1], predict_score(model, probes[::-1]))

    def test_cache_reuses_only_identical_included_augmentation(self):
        records, groups = [record('a'), record('b', 1)], {'a': augmented()}
        with tempfile.TemporaryDirectory() as directory:
            first = FoldFitter(groups, directory); model = first.fit(records, 'held', .1)
            second = FoldFitter(groups, directory)
            self.assertEqual(model, second.fit(records, 'held', .1)); self.assertEqual(second.disk_hits, 1)
            changed = copy.deepcopy(groups); changed['a']['features'][0, 0] += 1
            third = FoldFitter(changed, directory); third.fit(records, 'held', .1)
            self.assertEqual(third.fits, 1)


if __name__ == '__main__':
    unittest.main()
