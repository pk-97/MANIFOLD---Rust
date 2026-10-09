import copy
import unittest
from unittest.mock import Mock

import numpy as np

from .kick_fusion_calibration import THRESHOLDS, calibration_counts, choose_threshold, threshold_scores
from .kick_fusion_fine_calibration import refine_outer
from .run_kick_zero_core_trial import verify_inner_counts, zero_core_coarse


def evaluate(source, times):
    matched = int(any(abs(t-.11)<1e-9 for t in times))
    extra = int(any(abs(t-.31)<1e-9 for t in times))
    return [dict(labels=1, accuracy_by_tolerance_ms={str(ms):dict(matched=matched,missed=1-matched,extra=0) for ms in (50,70)},
                 association_early_35_late_200_ms=dict(matched=matched,missed=1-matched,extra=0)),
            dict(labels=0, accuracy_by_tolerance_ms={str(ms):dict(matched=0,missed=0,extra=extra) for ms in (50,70)},
                 association_early_35_late_200_ms=dict(matched=0,missed=0,extra=extra))]


def fixtures():
    records=[dict(track=n, features=np.array([[.82],[.805]]), available=np.array([10,30]),
                  sample_rate=1000,hop=10,source=dict(track=n)) for n in ('a','b','outer')]
    folds=[dict(validation_track=n,model=dict(training_tracks=[m for m in ('a','b') if m!=n]))
           for n in ('a','b')]
    budget=dict(matched_70ms=2,wide_extras=2,kick_free_extras=2)
    rows=[]
    for threshold in THRESHOLDS:
        counts=calibration_counts([p for r in records[:2] for p in
            threshold_scores(r,r['features'][:,0],threshold,evaluate)[1]])
        rows.append(dict(threshold=threshold,**counts,feasible=True))
    selected=choose_threshold(rows,budget)
    coarse=dict(outer_track='outer',threshold=selected['threshold'],threshold_selection_tracks=['a','b'],
        inner_baseline=budget,selected_inner_counts=selected,threshold_trials=rows,inner_folds=folds)
    return records,coarse


def predict(model,features):
    return features[:,0]


class ZeroCoreTests(unittest.TestCase):
    def test_zero_core_changes_only_feasibility_and_preserves_wide_budget(self):
        _,old=fixtures(); snapshot=copy.deepcopy(old)
        replacement={.7:(9,3,0),.8:(8,2,1),.9:(7,2,0)}
        for row in old['threshold_trials']:
            m,w,c=replacement.get(row['threshold'],(0,0,0))
            row.update(matched_70ms=m,wide_extras=w,kick_free_extras=c)
        before=copy.deepcopy(old)
        coarse=zero_core_coarse(old)
        self.assertEqual(old,before)
        self.assertEqual(coarse['threshold'],.9)
        self.assertEqual(coarse['inner_baseline']['wide_extras'],snapshot['inner_baseline']['wide_extras'])
        self.assertEqual(coarse['inner_baseline']['kick_free_extras'],0)
        self.assertEqual(coarse['original_inner_baseline'],old['inner_baseline'])
        for old_row,new_row in zip(old['threshold_trials'],coarse['threshold_trials']):
            self.assertEqual({k:v for k,v in old_row.items() if k!='feasible'},
                             {k:v for k,v in new_row.items() if k!='feasible'})
            self.assertEqual(new_row['feasible'],new_row['wide_extras']<=2 and new_row['kick_free_extras']==0)

    def test_refinement_uses_new_bracket_and_independently_verified_zero_counts(self):
        records,old=fixtures(); coarse=zero_core_coarse(old)
        self.assertEqual(old['threshold'],.8)
        self.assertEqual(coarse['threshold'],1.01)
        refined=refine_outer(records,coarse,predict=predict,evaluator=evaluate)
        self.assertEqual(refined['bracket'],[.8,1.01])
        self.assertEqual(len(refined['added_thresholds']),51)
        self.assertLessEqual(refined['threshold'],.82)
        self.assertGreater(refined['threshold'],.805)
        self.assertEqual(refined['selected_inner_counts']['matched_70ms'],2)
        for result in (coarse,refined):
            observed=verify_inner_counts(records,coarse,result['selected_inner_counts'],predict,evaluate)
            self.assertEqual(observed['kick_free_extras'],0)
            self.assertEqual(result['inner_baseline'],coarse['inner_baseline'])
        original_rows={r['threshold']:r for r in coarse['threshold_trials']}
        for row in refined['threshold_trials']:
            self.assertEqual(row['feasible'],row['wide_extras']<=2 and row['kick_free_extras']==0)
            if row['threshold'] in original_rows:
                self.assertEqual(row,original_rows[row['threshold']])

    def test_held_out_perturbation_cannot_change_refinement_or_count_verification(self):
        records,old=fixtures(); coarse=zero_core_coarse(old)
        evaluator=Mock(wraps=evaluate)
        first=refine_outer(records,coarse,predict=predict,evaluator=evaluator)
        records[-1].update(features=None,available=None,sample_rate=None,source=None)
        second=refine_outer(records,coarse,predict=predict,evaluator=evaluator)
        self.assertEqual(first,second)
        verify_inner_counts(records,coarse,second['selected_inner_counts'],predict,evaluator)
        self.assertTrue(all(c.args[0]['track']!='outer' for c in evaluator.call_args_list))

    def test_leaked_models_stale_counts_and_changed_grids_fail(self):
        records,old=fixtures(); coarse=zero_core_coarse(old)
        selected=copy.deepcopy(coarse['selected_inner_counts'])
        selected['matched_70ms']+=1
        with self.assertRaisesRegex(ValueError,'independent replay'):
            verify_inner_counts(records,coarse,selected,predict,evaluate)
        coarse['inner_folds'][0]['model']['training_tracks'].append('outer')
        with self.assertRaisesRegex(ValueError,'exclusions'):
            verify_inner_counts(records,coarse,coarse['selected_inner_counts'],predict,evaluate)
        old['threshold_trials'].pop()
        with self.assertRaisesRegex(ValueError,'coarse grid'):
            zero_core_coarse(old)


if __name__=='__main__':
    unittest.main()
