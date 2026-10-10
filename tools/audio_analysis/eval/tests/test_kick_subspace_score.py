import copy
import json
import unittest

import numpy as np

from eval.kick_subspace_score import _residual, _training_data, fit_without, predict_score
from eval.run_kick_fusion_trial import fit_without as fit_linear


def record(name, features, labels, ids=None):
    labels = np.asarray(labels, dtype=int)
    if ids is None:
        ids = np.where(labels == 1, np.arange(len(labels)), -1)
    return dict(track=name, features=np.asarray(features, dtype=float), labels=labels,
                event_ids=np.asarray(ids, dtype=int), training_mask=np.ones(len(labels), dtype=bool))


def planes(name):
    negative = [[0, 0, 1, 0], [0, 0, -1, 0], [0, 0, 0, 1], [0, 0, 0, -1]]
    positive = [[1, 0, 0, 0], [-1, 0, 0, 0], [0, 1, 0, 0], [0, -1, 0, 0]]
    return record(name, negative + positive, [0]*4 + [1]*4)


def weighted_records():
    first = record('a', [[-2, 1, 0, 0], [1, -1, 0, 1], [2, 2, 1, -1],
                         [1, 0, 3, 1], [2, 1, 4, 0], [1, 3, 2, 3]],
                   [0, 0, 0, 1, 1, 1], [-1, -1, -1, 0, 0, 1])
    second = record('b', [[-1, 2, 2, 0], [2, -2, 0, 3], [3, 1, 2, -2], [-1, -3, 1, 1],
                          [0, 1, 2, 2], [1, 2, 3, 1], [2, 4, 1, 3]],
                    [0, 0, 0, 0, 1, 1, 1], [-1, -1, -1, -1, 0, 1, 1])
    return [first, second]


class SubspaceTests(unittest.TestCase):
    def test_held_out_data_never_affect_normalisation_or_bases(self):
        records = [planes('a'), planes('held')]
        model = fit_without(records, 'held')
        records[-1].update(features=np.array([np.nan]), labels=None, event_ids=None, training_mask=None)
        self.assertEqual(model, fit_without(records, 'held'))
        self.assertEqual(model['training_tracks'], ['a'])

    def test_distinct_known_rank_two_subspaces_classify(self):
        model = fit_without([planes('a')], 'held')
        scores = predict_score(model, [[.3, -.8, 0, 0], [0, 0, .7, -.4]])
        self.assertGreater(scores[0], 1 - 1e-10)
        self.assertLess(scores[1], 1e-10)
        self.assertEqual(model, json.loads(json.dumps(model)))

    def test_frozen_song_class_event_weights_and_weighted_svd_math(self):
        records = weighted_records()
        _, x, y, weights = _training_data(records, 'held')
        expected = np.array([1/12]*3 + [1/16, 1/16, 1/8]
                            + [1/16]*4 + [1/8, 1/16, 1/16])
        np.testing.assert_allclose(weights, expected, rtol=0, atol=0)
        model, linear = fit_without(records, 'held'), fit_linear(records, 'held')
        np.testing.assert_allclose(model['mean'], linear['mean'], rtol=0, atol=0)
        np.testing.assert_allclose(model['scale'], linear['scale'], rtol=0, atol=0)
        mean = np.sum(x * expected[:, None], axis=0)
        scale = np.maximum(np.sqrt(np.sum((x-mean)**2 * expected[:, None], axis=0)), 1e-3)
        z = np.clip((x-mean)/scale, -8, 8)
        for label, name in ((0, 'negative'), (1, 'positive')):
            selected = y == label
            w = expected[selected] / expected[selected].sum()
            centre = np.sum(z[selected] * w[:, None], axis=0)
            centred = z[selected] - centre
            covariance = centred.T @ (centred * w[:, None])
            eigenvalues, eigenvectors = np.linalg.eigh(covariance)
            learned = model['classes'][name]
            np.testing.assert_allclose(learned['mean'], centre, atol=1e-12)
            np.testing.assert_allclose(np.array(learned['singular_values'])**2,
                                       eigenvalues[::-1], atol=1e-12)
            basis = np.array(learned['basis'])
            expected_basis = eigenvectors[:, -2:]
            np.testing.assert_allclose(basis.T @ basis, expected_basis @ expected_basis.T, atol=1e-12)

    def test_projection_reconstructs_and_residual_matches_explicit_error(self):
        model = fit_without([planes('a')], 'held')
        learned = model['classes']['positive']
        z = np.array([[1., -2., 3., 4.], [2., 1., 0., 0.]])
        basis, mean = np.array(learned['basis']), np.array(learned['mean'])
        reconstructed = ((z-mean) @ basis.T) @ basis + mean
        np.testing.assert_allclose(reconstructed, [[1, -2, 0, 0], [2, 1, 0, 0]], atol=1e-12)
        np.testing.assert_allclose(basis @ basis.T, np.eye(2), atol=1e-12)
        np.testing.assert_allclose(_residual(z, learned, 2), [25, 0], atol=1e-12)
        self.assertAlmostEqual(predict_score(model, [[1, 0, 2, 0]])[0], .2, places=10)

    def test_both_zero_residuals_are_finite_neutral_and_empty_input_works(self):
        model = fit_without([planes('a')], 'held')
        self.assertEqual(predict_score(model, np.zeros((1, 4))).tolist(), [.5])
        model['classes']['negative'] = copy.deepcopy(model['classes']['positive'])
        scores = predict_score(model, [[1, 2, 0, 0], [0, 0, 0, 0]])
        np.testing.assert_array_equal(scores, [.5, .5])
        self.assertEqual(predict_score(model, np.empty((0, 4))).shape, (0,))

    def test_only_reviewed_training_samples_affect_fit(self):
        first = planes('a')
        first['training_mask'][-1] = False
        model = fit_without([first], 'held')
        first['features'][-1] = np.nan
        first['labels'][-1] = 3
        self.assertEqual(model, fit_without([first], 'held'))

    def test_scale_floor_and_prediction_clipping(self):
        first = planes('a')
        first['features'] = np.column_stack((first['features'], np.ones(8)))
        model = fit_without([first], 'held')
        self.assertEqual(model['scale'][-1], 1e-3)
        score = predict_score(model, [[1e100, -1e100, 0, 0, 1.]])
        equivalent = np.array(model['mean']) + np.array(model['scale']) * [8, -8, 0, 0, 0]
        np.testing.assert_allclose(score, predict_score(model, [equivalent]), atol=0)

    def test_invalid_fit_inputs_fail_explicitly(self):
        for rank in (0, -1, 1.5, True, 4, 5):
            with self.subTest(rank=rank), self.assertRaises(ValueError):
                fit_without([planes('a')], 'held', rank=rank)
        with self.assertRaisesRegex(ValueError, 'unique'):
            fit_without([planes('a'), planes('a')], 'held')
        with self.assertRaisesRegex(ValueError, 'no training'):
            fit_without([planes('held')], 'held')
        bad = planes('a')
        bad['features'][0, 0] = np.nan
        with self.assertRaisesRegex(ValueError, 'finite'):
            fit_without([bad], 'held')
        bad = planes('a')
        bad['labels'][:] = 0
        with self.assertRaisesRegex(ValueError, 'both training classes'):
            fit_without([bad], 'held')
        bad = planes('a')
        bad['training_mask'][:3] = False
        with self.assertRaisesRegex(ValueError, 'more samples'):
            fit_without([bad], 'held')
        bad = planes('a')
        bad['features'][:] = 1
        with self.assertRaisesRegex(ValueError, 'numerical rank'):
            fit_without([bad], 'held')
        bad = planes('a')
        bad['event_ids'][-1] = -1
        with self.assertRaisesRegex(ValueError, 'event IDs'):
            fit_without([bad], 'held')

    def test_invalid_prediction_inputs_fail_explicitly(self):
        model = fit_without([planes('a')], 'held')
        for x in ([[np.nan]*4], [[np.inf]*4], [[0]*3], [0]*4):
            with self.subTest(features=x), self.assertRaises(ValueError):
                predict_score(model, x)
        model['scale'][0] = 0
        with self.assertRaisesRegex(ValueError, 'positive scale'):
            predict_score(model, [[0]*4])


if __name__ == '__main__':
    unittest.main()
