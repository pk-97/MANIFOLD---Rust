"""Small proofs for covariance compilation, fold exclusion and causal scoring."""
import copy
import unittest

import numpy as np

from .kick_covariance_score import FoldFitter, decision_logits, fit_without, predict_score
from .run_kick_fusion_trial import fit_without as linear_fit
from .test_kick_hard_negative_weights import records


class CovarianceTests(unittest.TestCase):
    def test_compiled_quadratic_equals_direct_gaussian(self):
        data = records()
        for shrinkage in (.1, .5, .9):
            model = fit_without(data, 'song0', shrinkage)
            x = data[0]['features']
            z = np.clip((x-model['mean'])/model['scale'], -8, 8)
            likelihood = []
            for c in model['classes']:
                delta = z-c['mean']
                likelihood.append(-.5*(np.einsum('bi,ij,bj->b', delta,
                    np.linalg.inv(c['covariance']), delta)+np.linalg.slogdet(c['covariance'])[1]))
            np.testing.assert_allclose(decision_logits(model, x),
                (likelihood[0]-likelihood[1])/x.shape[1], rtol=1e-12, atol=1e-12)

    def test_preprocessing_matches_frozen_baseline(self):
        data = records()
        model = fit_without(data, 'song0', .5)
        baseline = linear_fit(data, 'song0')
        self.assertEqual(model['mean'], baseline['mean'])
        self.assertEqual(model['scale'], baseline['scale'])

    def test_outer_exclusion_and_training_cache(self):
        data = records()
        fitter = FoldFitter(.5)
        first = fitter.fit(data, 'song0')
        changed = copy.deepcopy(data)
        changed[0]['features'] *= 1000
        changed[0]['labels'] = 1-changed[0]['labels']
        self.assertEqual(first, fitter.fit(changed, 'song0'))
        self.assertEqual(len(fitter.cache), 1)
        changed[1]['features'][0] += 10
        second = fitter.fit(changed, 'song0')
        self.assertEqual(second, FoldFitter(.5).fit(changed, 'song0'))
        self.assertNotEqual(first['quadratic'], second['quadratic'])

    def test_prediction_is_row_local_and_finite_with_constant_column(self):
        data = records()
        for row in data:
            row['features'][:, 2] = 0
        model = fit_without(data, 'song0', .1)
        x = data[0]['features']
        full = predict_score(model, x)
        self.assertTrue(np.isfinite(full).all())
        for i in range(len(x)):
            np.testing.assert_allclose(predict_score(model, x[i:i+1]), full[i:i+1], atol=1e-15)
        for c in model['classes']:
            self.assertGreaterEqual(np.linalg.eigvalsh(c['covariance']).min(), .00999999)

    def test_covariance_separates_equal_mean_crossed_axes(self):
        data = records()
        for row in data:
            row['labels'] = np.repeat([1, 0], 12)
            row['event_ids'] = np.repeat([0, -1], 12)
            row['features'] = np.zeros((len(row['labels']), 3))
            for label, direction in ((1, 1), (0, -1)):
                indices = np.flatnonzero(row['labels'] == label)
                signs = np.where(np.arange(len(indices)) % 2, -1, 1)
                row['features'][indices, :2] = np.column_stack((signs, direction*signs))
        model = fit_without(data, 'song0', .1)
        x = np.array([[1, 1, 0], [-1, -1, 0], [1, -1, 0], [-1, 1, 0]])
        scores = predict_score(model, x)
        self.assertTrue((scores[:2] > .9).all())
        self.assertTrue((scores[2:] < .1).all())


if __name__ == '__main__':
    unittest.main()
