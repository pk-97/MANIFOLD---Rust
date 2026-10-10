import copy
import unittest
from unittest.mock import Mock

import numpy as np

from eval.kick_fusion_calibration import THRESHOLDS, calibrate_outer, choose_threshold, nested_calibration, threshold_scores
from eval.kick_fusion_fine_calibration import build_report, refinement_grid, refine_outer
from eval.run_kick_dsp_experiments import evaluate
from eval.run_kick_fusion_trial import predict_score


def record(name, shift=0):
    source = dict(track=name, group='original_five', truth=[.06], regions=[])
    return dict(track=name, source=source, features=np.array([[-2.], [-1.], [1.], [2.]]) + shift,
                labels=np.array([0, 0, 1, 1]), event_ids=np.array([-1, -1, 0, 0]),
                training_mask=np.ones(4, bool), available=np.array([1, 3, 5, 7]),
                sample_rate=1000, hop=10, ref=dict(scores=dict(v5=evaluate(source, [.06]))))


class FineCalibrationTests(unittest.TestCase):
    def test_nearest_lower_infeasible_bracket_and_coarse_retention(self):
        coarse = dict(threshold=.95, threshold_trials=[dict(threshold=t, feasible=t >= .95)
                                                      for t in THRESHOLDS])
        grid, bracket = refinement_grid(coarse)
        self.assertEqual(bracket, [.925, .95])
        self.assertTrue(set(THRESHOLDS).issubset(grid))
        interior = [t for t in grid if t not in THRESHOLDS]
        self.assertEqual(len(interior), 51)
        self.assertEqual(interior, np.linspace(.925, .95, 53)[1:-1].tolist())
        self.assertTrue(all(.925 < t < .95 for t in interior))
        coarse['threshold_trials'][0]['feasible'] = True
        self.assertEqual(refinement_grid(coarse), (grid, bracket))

    def test_no_lower_infeasible_keeps_exact_coarse_grid(self):
        coarse = dict(threshold=.5, threshold_trials=[dict(threshold=t, feasible=True) for t in THRESHOLDS])
        self.assertEqual(refinement_grid(coarse), (list(THRESHOLDS), None))

    def test_outer_perturbation_cannot_change_bracket_predictions_or_winner(self):
        records = [record(str(i), i*.05) for i in range(9)]
        coarse = calibrate_outer(records, '8')
        evaluator = Mock(wraps=evaluate)
        first = refine_outer(records, coarse, evaluator=evaluator)
        changed = copy.deepcopy(records)
        changed[-1]['features'] *= 1e9
        changed[-1]['labels'] = 1 - changed[-1]['labels']
        changed[-1]['training_mask'][:] = False
        changed[-1]['source']['truth'] = [999.]
        changed[-1]['ref']['scores']['v5'][0]['accuracy_by_tolerance_ms']['70']['matched'] = 999
        updated_coarse = calibrate_outer(changed, '8')
        self.assertEqual(coarse, updated_coarse)
        self.assertEqual(first, refine_outer(changed, updated_coarse))
        self.assertTrue(all(call.args[0]['track'] != '8' for call in evaluator.call_args_list))
        self.assertTrue(set(THRESHOLDS).issubset(r['threshold'] for r in first['threshold_trials']))

    def test_refinement_rejects_inner_model_with_outer_song(self):
        records = [record(name) for name in ('a', 'b', 'c')]
        coarse = calibrate_outer(records, 'c')
        coarse['inner_folds'][0]['model']['training_tracks'].append('c')
        with self.assertRaises(ValueError):
            refine_outer(records, coarse)

    def test_unchanged_tie_order_uses_wide_extras_then_higher_cutoff(self):
        rows = [dict(threshold=t, matched_70ms=m, wide_extras=w, kick_free_extras=b)
                for t, m, w, b in [(.925, 7, 3, 0), (.926, 7, 2, 0),
                                   (.927, 7, 2, 0), (.928, 8, 2, 2)]]
        self.assertEqual(choose_threshold(rows, dict(wide_extras=3, kick_free_extras=1)), rows[2])

    def test_report_preserves_original_five_schema_and_label_identities(self):
        records = [record(name) for name in ('a', 'b', 'c')]
        coarse = dict(tracks=nested_calibration(records))
        original = copy.deepcopy(coarse)
        refinements = []
        for record_, row in zip(records, coarse['tracks']):
            calibration = refine_outer(records, row['calibration'])
            fires, scores = threshold_scores(record_, predict_score(row['model'], record_['features']),
                                             calibration['threshold'])
            refinements.append(dict(track=row['track'], calibration=calibration, kick_hops=fires, scores=scores))
        report = build_report(coarse, refinements)
        self.assertEqual(coarse, original)
        self.assertEqual(set(report['totals']), {'baseline', 'fixed_05', 'coarse', 'refined'})
        for row in report['tracks']:
            self.assertEqual(row['kick_hops']['coarse'], next(t for t in original['tracks']
                             if t['track'] == row['track'])['kick_hops']['calibrated'])
            self.assertEqual(set(row['coarse_to_refined'][0]['metrics']), {'35', '50', '70', 'association'})
            self.assertIn('lost_labels_s', row['comparisons']['refined'][0]['metrics']['50'])
        self.assertEqual(set(report['totals']['refined']['tolerance_ms']), {'35', '50', '70'})
        self.assertIn('bass_only_cores', report)


if __name__ == '__main__':
    unittest.main()
