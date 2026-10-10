import copy
import unittest
from unittest.mock import Mock

import numpy as np

from eval.kick_fusion_calibration import (
    THRESHOLDS, calibrate_outer, choose_threshold, nested_calibration, select_fires,
    threshold_scores,
)
from eval.run_kick_fusion_calibration import comparisons
from eval.run_kick_dsp_experiments import evaluate
from eval.run_kick_fusion_trial import fit_without, select_fires as frozen_select_fires


def record(name, shift=0):
    source = dict(track=name, group='original_five', truth=[.06], regions=[])
    return dict(track=name, source=source, features=np.array([[-2.], [-1.], [1.], [2.]]) + shift,
                labels=np.array([0, 0, 1, 1]), event_ids=np.array([-1, -1, 0, 0]),
                training_mask=np.ones(4, bool), available=np.array([1, 3, 5, 7]),
                sample_rate=1000, hop=10, ref=dict(scores=dict(v5=evaluate(source, [.06]))))


class CalibrationTests(unittest.TestCase):
    def test_original_five_strict_label_changes_without_pair_fields(self):
        source = dict(track='test', group='original_five', truth=[1., 2.], regions=[])
        before, after = evaluate(source, [1.]), evaluate(source, [2.])
        self.assertNotIn('pairs', before[0]['accuracy_by_tolerance_ms']['50'])
        changes = comparisons(before, after)[0]['metrics']
        for key in ('35', '50', '70', 'association'):
            self.assertEqual(changes[key], dict(lost_labels_s=[1.], recovered_labels_s=[2.]))

    def test_grid_is_exact_and_includes_silence(self):
        self.assertEqual(THRESHOLDS, tuple(round(.5 + .025*i, 3) for i in range(20))
                         + (.99, .999, 1.01))
        self.assertEqual(select_fires([1., .9999], [10, 20], 1000, 10, 1.01), [])

    def test_ties_use_fewer_wide_extras_then_higher_threshold(self):
        rows = [dict(threshold=t, matched_70ms=m, wide_extras=w, kick_free_extras=b)
                for t, m, w, b in [(.5, 9, 4, 0), (.6, 8, 2, 1), (.7, 8, 2, 0),
                                   (.8, 8, 3, 0), (.9, 10, 1, 2), (1.01, 0, 0, 0)]]
        self.assertEqual(choose_threshold(rows, dict(wide_extras=3, kick_free_extras=1))['threshold'], .7)

    def test_outer_excluded_from_inner_fits_normalisation_and_threshold_choice(self):
        records = [record(str(i), i*.05) for i in range(9)]
        fit, evaluator = Mock(wraps=fit_without), Mock(wraps=evaluate)
        first = calibrate_outer(records, '8', fit=fit, evaluator=evaluator)
        self.assertEqual(fit.call_count, 8)
        for call, fold in zip(fit.call_args_list, first['inner_folds']):
            self.assertNotIn('8', [r['track'] for r in call.args[0]])
            self.assertEqual(len(fold['model']['training_tracks']), 7)
            self.assertNotIn(fold['validation_track'], fold['model']['training_tracks'])
        self.assertTrue(all(call.args[0]['track'] != '8' for call in evaluator.call_args_list))
        changed = copy.deepcopy(records)
        changed[-1]['features'] *= 1e9
        changed[-1]['labels'] = 1 - changed[-1]['labels']
        changed[-1]['training_mask'][:] = False
        changed[-1]['source']['truth'] = [999.]
        changed[-1]['ref']['scores']['v5'][0]['accuracy_by_tolerance_ms']['70']['matched'] = 999
        self.assertEqual(first, calibrate_outer(changed, '8'))
        self.assertNotIn('8', first['threshold_selection_tracks'])

    def test_outer_model_excludes_outer_and_uses_selected_threshold_once(self):
        records = [record(name) for name in ('a', 'b', 'c')]
        folds = nested_calibration(records)
        for record_, fold in zip(records, folds):
            self.assertNotIn(fold['track'], fold['model']['training_tracks'])
            self.assertEqual(len(fold['model']['training_tracks']), 2)
            from eval.run_kick_fusion_trial import predict_score
            scores = predict_score(fold['model'], record_['features'])
            expected = select_fires(scores, record_['available'], 1000, 10,
                                    fold['calibration']['threshold'])
            self.assertEqual(expected, fold['kick_hops']['calibrated'])

    def test_refractory_and_scoring_use_actual_available_hop_end(self):
        scores, available = [.7, .9, .4, .8], np.array([8, 10, 13, 15])
        self.assertEqual(select_fires(scores, available, 1000, 10, .5), [8, 15])
        self.assertEqual(select_fires(scores, available, 1000, 10, .5),
                         frozen_select_fires(scores, available, 1000, 10))
        r = record('a')
        r['available'] = available
        evaluator = Mock(return_value=[])
        threshold_scores(r, scores, .5, evaluator)
        self.assertEqual(evaluator.call_args.args[1], [.09, .16])
        self.assertEqual(select_fires([.9, .9], [8, 14], 1000, 10, .5), [8, 14])

    def test_rejects_misaligned_or_nonchronological_predictions(self):
        with self.assertRaises(ValueError):
            select_fires([.9], [8, 14], 1000, 10, .5)
        with self.assertRaises(ValueError):
            select_fires([.9, .9], [14, 8], 1000, 10, .5)


if __name__ == '__main__':
    unittest.main()
