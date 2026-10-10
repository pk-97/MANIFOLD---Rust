"""Event-mass conservation, deadline eligibility and fold-local latent selection."""
import copy
import unittest

import numpy as np

from .kick_event_window_score import FoldFitter, event_weights, fit_without, timely_positive
from .run_kick_fusion_trial import fit_without as linear_fit, predict_score


def records():
    rng = np.random.default_rng(23)
    result = []
    for i in range(4):
        truth = np.arange(1, 5.)
        y = np.tile([1, 1, 1, 0, 0, 0], 4)
        ids = np.concatenate([[j, j, j, -1, -1, -1] for j in range(4)])
        starts = np.concatenate([1000*t+np.array([-21, -1, 30, 200, 400, 600]) for t in truth]).astype(int)
        result.append(dict(track=f'song{i}', features=rng.normal(size=(24, 3))+y[:, None],
            labels=y, event_ids=ids, candidates=starts, available=starts+40,
            training_mask=np.ones(24, bool), sample_rate=1000, hop=1,
            source=dict(group='original_five', truth=truth.tolist(), regions=[])))
    return result


class WindowTests(unittest.TestCase):
    def test_event_weights_preserve_mass_and_ignore_untimely_windows(self):
        r = records()[0]
        timely = timely_positive(r)
        logits = np.tile([0., 1., 10., 0., 0., 0.], 4)
        for name in ('top1', 'top2', 'softmax_tau1'):
            w = event_weights(r['labels'], r['event_ids'], timely, logits, name)
            self.assertAlmostEqual(w[r['labels'] == 0].sum(), .5)
            self.assertAlmostEqual(w[r['labels'] == 1].sum(), .5)
            np.testing.assert_array_equal(w[np.arange(2, 24, 6)], 0)
            for event in range(4):
                self.assertAlmostEqual(w[r['event_ids'] == event].sum(), .125)
        w = event_weights(r['labels'], r['event_ids'], timely, logits, 'top1')
        np.testing.assert_allclose(w[np.arange(1, 24, 6)], .125)

    def test_deadline_boundary_uses_emission_not_candidate_time(self):
        r = records()[0]
        self.assertFalse(timely_positive(r)[2])  # Emits71ms after truth.
        r['available'][2] -= 1
        self.assertTrue(timely_positive(r)[2])   # Exactly70ms is eligible.

    def test_stable_ties_and_soft_selection(self):
        y, ids = np.array([1, 1, 0]), np.array([0, 0, -1])
        w = event_weights(y, ids, [1, 1, 0], [2, 2, 0], 'top1')
        np.testing.assert_array_equal(w, [.5, 0, .5])
        w = event_weights(y, ids, [1, 1, 0], [0, np.log(3), 0], 'softmax_tau1')
        np.testing.assert_allclose(w, [.125, .375, .5])

    def test_fitting_excludes_held_out_and_freezes_preprocessing(self):
        data = records(); fitter = FoldFitter('top2')
        model = fitter.fit(data, 'song0'); baseline = linear_fit(data, 'song0')
        self.assertEqual(model['mean'], baseline['mean'])
        self.assertEqual(model['scale'], baseline['scale'])
        changed = copy.deepcopy(data)
        changed[0]['features'] *= 1000; changed[0]['source']['truth'] = []
        self.assertEqual(model, fitter.fit(changed, 'song0'))
        changed[1]['features'][0] += 20
        other = fitter.fit(changed, 'song0')
        self.assertEqual(other, fit_without(changed, 'song0', 'top2'))
        self.assertNotEqual(other['weights'], model['weights'])
        full = predict_score(model, data[0]['features'])
        for i in range(len(full)):
            np.testing.assert_allclose(predict_score(model, data[0]['features'][i:i+1]), full[i:i+1])


if __name__ == '__main__':
    unittest.main()
