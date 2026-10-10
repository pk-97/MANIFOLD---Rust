"""Weight conservation, baseline equivalence and exclusion proofs on small data."""
import copy
import unittest

import numpy as np

from .kick_fusion_calibration import calibrate_outer
from .kick_hard_negative_weights import FoldFitter, reviewed_masks, song_weights, temporal_eligibility
from .run_kick_hard_negative_trial import rank_diagnostics
from .run_kick_fusion_trial import fit_without, predict_score


def fake_score(extras=0):
    return dict(labels=0, accuracy_by_tolerance_ms={str(t): dict(matched=0, missed=0, extra=extras)
                for t in (35, 50, 70)}, association_early_35_late_200_ms=dict(
                    matched=0, unmatched_labels=0, unmatched_triggers=extras))


def records():
    rng = np.random.default_rng(11)
    result = []
    for i in range(4):
        y = np.tile([1, 1, 1, 0, 0, 0, 0, 0], 3)
        x = rng.normal(size=(len(y), 3)) + y[:, None]*2
        # A convincing negative creates a nonempty hard subset for some folds.
        x[3] = 7
        candidates = np.arange(len(y))*100
        source = dict(group='original_five', truth=[], regions=[])
        score = fake_score()
        result.append(dict(track=f'song{i}', features=x, labels=y,
            event_ids=np.tile([0, 0, 1, -1, -1, -1, -1, -1], 3),
            training_mask=np.ones(len(y), bool), candidates=candidates,
            available=candidates+8, hop=1, sample_rate=1000, source=source,
            ref=dict(scores=dict(v5=[score]))))
    return result


class WeightTests(unittest.TestCase):
    def test_conserves_classes_and_positive_events(self):
        y = np.array([1, 1, 1, 0, 0, 0])
        ids = np.array([4, 4, 9, -1, -1, -1])
        hard = np.array([0, 0, 0, 1, 0, 0], bool)
        for multiplier in (1, 2, 4):
            w = song_weights(y, ids, hard, multiplier)
            np.testing.assert_array_equal(w[:3], [.125, .125, .25])
            self.assertAlmostEqual(w[3:].sum(), .5)
            self.assertAlmostEqual(w[3]/w[4], multiplier)
            self.assertAlmostEqual(w[3], .5*multiplier/(2+multiplier))
        for hard in (np.zeros(6, bool), y == 0):
            np.testing.assert_array_equal(song_weights(y, ids, hard, 1),
                                          song_weights(y, ids, hard, 4))

    def test_exact_baseline_and_fixed_preprocessing(self):
        data = records()
        fitter = FoldFitter({r['track']: r['labels'] == 0 for r in data})
        baseline = fit_without(data, 'song0')
        selected = []
        for multiplier in (1, 2, 4):
            model = fitter.fit(data, 'song0', multiplier)
            self.assertEqual(model['mean'], baseline['mean'])
            self.assertEqual(model['scale'], baseline['scale'])
            self.assertNotIn('song0', model['training_tracks'])
            selected.append([s['selected_hops'] for s in model['selection']])
            for song in model['selection']:
                self.assertAlmostEqual(song['positive_mass'], 1/6)
                self.assertAlmostEqual(song['negative_mass'], 1/6)
            if multiplier == 1:
                np.testing.assert_array_equal(predict_score(model, data[0]['features']),
                                              predict_score(baseline, data[0]['features']))
        self.assertEqual(selected[0], selected[1])
        self.assertEqual(selected[1], selected[2])
        self.assertTrue(any(selected[0]))

    def test_temporal_guard_includes_context_and_ignores_positive_rows(self):
        row = records()[0]
        row.update(candidates=np.array([764, 765, 964, 1200, 1300]),
                   available=np.array([964, 965, 1000, 1208, 1308]),
                   labels=np.array([0, 0, 0, 0, 1]), training_mask=np.ones(5, bool))
        row['source']['truth'] = [1.0]
        # End965ms touches the guard; candidate1201ms clears the tail.
        np.testing.assert_array_equal(temporal_eligibility(row), [False, False, False, True, False])

    def test_outer_perturbation_cannot_change_fit_or_calibration(self):
        data = records()
        changed = copy.deepcopy(data)
        changed[0]['features'] *= 1000
        changed[0]['labels'] = 1-changed[0]['labels']
        changed[0]['ref']['scores']['v5'][0]['association_early_35_late_200_ms']['unmatched_triggers'] = 1000

        def evaluate(source, times):
            return [fake_score(len(times))]

        results = []
        for rows in (data, changed):
            fitter = FoldFitter({r['track']: r['labels'] == 0 for r in rows})
            fit = lambda r, held: fitter.fit(r, held, 2)
            results.append((fit(rows, 'song0'), calibrate_outer(rows, 'song0', fit=fit, evaluator=evaluate)))
        self.assertEqual(results[0], results[1])
        # Independently changing an inner validation song must not change that fit.
        changed[1]['features'] *= -17
        a = FoldFitter({r['track']: r['labels'] == 0 for r in data})
        b = FoldFitter({r['track']: r['labels'] == 0 for r in changed})
        self.assertEqual(a.fit(data[1:], 'song1', 4), b.fit(changed[1:], 'song1', 4))

    def test_invalid_weights_fail(self):
        for y, ids, hard, multiplier in (([1, 0], [0, -1], [1, 0], 2),
                                         ([1, 0], [0, -1], [0, 1], 3),
                                         ([0, 0], [-1, -1], [0, 1], 2)):
            with self.assertRaises(ValueError):
                song_weights(y, ids, hard, multiplier)

    def test_training_cache_invalidates_on_feature_or_review_change(self):
        data = records()
        reviewed = {r['track']: r['labels'] == 0 for r in data}
        fitter = FoldFitter(reviewed)
        first = fitter.fit(data, 'song0', 4)
        data[1]['features'][3] = -7
        second = fitter.fit(data, 'song0', 4)
        fresh = FoldFitter(reviewed).fit(data, 'song0', 4)
        self.assertEqual(second, fresh)
        self.assertNotEqual(first['weights'], second['weights'])
        reviewed['song2'][:] = False
        self.assertEqual(fitter.fit(data, 'song0', 4), FoldFitter(reviewed).fit(data, 'song0', 4))

    def test_evidence_identity_and_temporal_guards(self):
        data = records()
        review = dict(status='lead_accepted_provisional_source_review', tracks=[])
        for row in data:
            row['source']['audio_sha256'] = row['track']
            review['tracks'].append(dict(track=row['track'], audio_sha256=row['track'], candidates=[
                dict(candidate_index=3, candidate_hop=300, disposition='accept_non_kick',
                     reason='hand-labelled synthetic negative', evidence=['synthetic case'])]))
        self.assertTrue(reviewed_masks(data, review)['song0'][3])
        review['tracks'][0]['candidates'][0]['candidate_hop'] = 301
        with self.assertRaisesRegex(ValueError, 'identity'):
            reviewed_masks(data, review)
        review['tracks'][0]['candidates'][0]['candidate_hop'] = 300
        data[0]['source']['truth'] = [.300]
        with self.assertRaisesRegex(ValueError, 'eligibility'):
            reviewed_masks(data, review)

    def test_rank_diagnostic_distinguishes_score_shift_from_ordering(self):
        record = records()[0]
        model = dict(mean=[0, 0, 0], scale=[1, 1, 1], weights=[1, 0, 0], intercept=0)
        shifted = dict(model, intercept=-3)
        a = rank_diagnostics(record, model, model)
        b = rank_diagnostics(record, shifted, model)
        self.assertEqual(a['event_balanced_candidate_auc'], b['event_balanced_candidate_auc'])
        self.assertAlmostEqual(a['event_balanced_mean_logit_margin'], b['event_balanced_mean_logit_margin'])
        np.testing.assert_allclose(b['positive_paired_logit_change'], [-3, -3, -3])


if __name__ == '__main__':
    unittest.main()
