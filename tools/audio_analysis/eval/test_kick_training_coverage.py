"""Focused checks of training coverage isolation and existing boundary semantics."""
import copy
import unittest

import numpy as np

from .kick_boosted_score import training_key
from .kick_subspace_score import _training_data
from .kick_training_coverage import add_training_coverage, merge_training_cores


def record(times=(.1,.5,1.,1.36,1.4,1.95,1.96,2.04)):
    candidates = np.rint(np.asarray(times)*1000).astype(int)-1
    mask = np.zeros(len(times),bool); mask[:2] = True
    labels = np.zeros(len(times),int); labels[0] = 1
    ids = np.full(len(times),-1,int); ids[0] = 7
    return dict(track='family',source={'original':True},ref={'original':True},
        candidates=candidates,available=candidates+40,sample_rate=1000,hop=1,
        features=np.arange(len(times)*15,dtype=float).reshape(-1,15),
        training_mask=mask,labels=labels,event_ids=ids,
        cache_metadata=dict(duration_s=3.,signature=dict(audio_sha256='abc')))


def core(**changes):
    return dict(dict(id='extra',start_s=1.,end_s=2.,review_start_s=.8,
        review_end_s=2.2,kick_times_s=[1.4],uncertain_regions=[],scoring_ready=True),**changes)


class CoverageTests(unittest.TestCase):
    def test_train_only_merge_preserves_old_rows_and_event_identity(self):
        original = record(); snapshot = copy.deepcopy(original)
        merged, receipt = merge_training_cores(original,[core()])
        np.testing.assert_array_equal(np.flatnonzero(merged['training_mask']),[0,1,2,3,4,5])
        np.testing.assert_array_equal(merged['event_ids'][[0,4]],[7,8])
        self.assertIs(merged['source'],original['source']); self.assertIs(merged['ref'],original['ref'])
        for name in ('features','training_mask','labels','event_ids'):
            np.testing.assert_array_equal(original[name],snapshot[name])
        self.assertEqual(receipt['added_rows'],4); self.assertEqual(receipt['added_positive_rows'],1)
        self.assertEqual(receipt['added_represented_events'],1)

    def test_completed_window_margin_truth_and_uncertainty(self):
        original = record((.1,.5,1.,1.4,1.49,1.82,1.98))
        passage = core(kick_times_s=[.99,1.4,2.01],
            uncertain_regions=[dict(start_s=1.6,end_s=1.61,reason='ambiguous')])
        merged, receipt = merge_training_cores(original,[passage])
        # Margin onset cannot become a negative; evidence intersecting uncertainty
        # is excluded, and evidence ending beyond the core is unavailable to train.
        self.assertEqual(receipt['added_row_indices'],[3,5])
        self.assertEqual(merged['event_ids'][3],9)
        self.assertAlmostEqual(receipt['accepted_cores'][0]['excluded_regions'][0]['start_s'],1.53)

    def test_opening_and_unscorable_cores(self):
        original = record((.01,.1,.2,.241,.8)); original['training_mask'][:] = False
        opening = core(start_s=0.,review_start_s=0.,kick_times_s=[],
            raw_excerpt_coordinates=dict(master=dict(source_sample_range=[0,2250],master_axis_origin_s=0)),
            uncertain_regions=[dict(start_s=0.,end_s=.04,reason='startup uncertain')])
        unknown = dict(id='unknown',scoring_ready=False,reason='unknown truth')
        merged, receipt = merge_training_cores(original,[opening,unknown])
        self.assertEqual(receipt['added_row_indices'],[3,4])
        self.assertEqual(receipt['added_positive_rows'],0)
        self.assertTrue(receipt['accepted_cores'][0]['recording_start_boundary'])
        self.assertEqual(len(receipt['unscored_cores']),1)
        opening['raw_excerpt_coordinates']['master']['source_sample_range'][0]=1
        with self.assertRaisesRegex(ValueError,'actual recording sample zero'):
            merge_training_cores(original,[opening])

    def test_zero_addition_and_rejected_overlap_or_provenance(self):
        original = record(); empty, receipt = merge_training_cores(original,[])
        self.assertEqual(training_key(empty),training_key(original)); self.assertEqual(receipt['added_rows'],0)
        overlapping = core(start_s=.1,review_start_s=-.1)
        with self.assertRaisesRegex(ValueError,'selected training rows overlap'):
            merge_training_cores(original,[overlapping])
        reviewed=dict(status='lead_reviewed_visual_provisional',tracks=[dict(track='family',audio_sha256='bad',cores=[core()])])
        with self.assertRaisesRegex(ValueError,'frozen feature source differ'):
            add_training_coverage([original],reviewed)

    def test_family_class_event_weights_and_heldout_exclusion(self):
        rows=[]
        for track in ('a','b','held'):
            r=record();r['track']=track; r,_=merge_training_cores(r,[core()]);rows.append(r)
        training,x,y,w=_training_data(rows,'held')
        self.assertEqual([r['track'] for r in training],['a','b'])
        np.testing.assert_allclose(w.sum(),1.)
        cursor=0
        for row in training:
            selected=row['training_mask']; size=int(selected.sum()); weights=w[cursor:cursor+size]
            targets=row['labels'][selected]; ids=row['event_ids'][selected]
            self.assertAlmostEqual(weights.sum(),.5)
            for label in (0,1): self.assertAlmostEqual(weights[targets==label].sum(),.25)
            for event in np.unique(ids[targets==1]): self.assertAlmostEqual(weights[ids==event].sum(),.125)
            cursor+=size
        rows[-1]['features'][:]=np.nan;rows[-1]['labels'][:]=22
        _,x2,y2,w2=_training_data(rows,'held')
        for a,b in ((x,x2),(y,y2),(w,w2)): np.testing.assert_array_equal(a,b)


if __name__ == '__main__':
    unittest.main()
