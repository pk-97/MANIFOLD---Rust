import unittest
from unittest.mock import patch
import numpy as np

from eval.run_kick_fusion_trial import fit_without, predict_score, select_fires, training_labels
from eval.kick_fusion_features import _rise_edges, fusion_features


def record(name,shift=0):
    return dict(track=name,features=np.array([[-2.+shift],[-1.+shift],[1.+shift],[2.+shift]]),
                labels=np.array([0,0,1,1]),event_ids=np.array([-1,-1,0,1]),
                training_mask=np.ones(4,bool))


class FusionScoreTests(unittest.TestCase):
    def test_excluded_onset_cannot_train_from_window_after_exclusion(self):
        source = dict(group='original_five', truth=[1., 1.2],
                      regions=[dict(start_s=.9, end_s=1.01)])
        # First window is fully outside exclusion but within35ms of its onset.
        mask, labels, ids = training_labels(source, {}, np.array([101, 119, 149]),
                                            np.array([105, 123, 153]), 1000, 10, 2.)
        self.assertEqual(mask.tolist(), [False, True, True])
        self.assertEqual(labels.tolist(), [0, 1, 0])
        self.assertEqual(ids.tolist(), [-1, 1, -1])

    def test_review_margin_onset_does_not_train_inside_core(self):
        passage = dict(start_s=1., end_s=2., kick_times_s=[.99, 1.2])
        source = dict(group='master', passages=[passage])
        ref = dict(scores=dict(v5=[dict(excluded_regions=[])]))
        mask, labels, _ = training_labels(source, ref, np.array([99, 119]),
                                          np.array([103, 123]), 1000, 10, 3.)
        self.assertEqual(mask.tolist(), [False, True])
        self.assertEqual(labels.tolist(), [0, 1])

    def test_new_body_attack_is_proposed_while_low_stays_active(self):
        base=np.ones((3,3,2));upper=np.ones((3,4,2))
        base[:,0,0]=2; base[1,1,0]=3
        self.assertEqual(_rise_edges(base,upper).tolist(),[True,True,False])

    def test_rise_evidence_can_arrive_after_first_15ms(self):
        base=np.ones((12,3,2));upper=np.ones((12,4,2))
        base[:,0,0]=2; base[4,0,0]=12
        with patch('eval.kick_fusion_features.causal_features',return_value=(base,256)), \
             patch('eval.kick_fusion_features.upper_features',return_value=(upper,256)):
            candidates,available,x,_=fusion_features(np.ones(12*256),48000)
        self.assertEqual(candidates.tolist(),[0])
        self.assertEqual(available.tolist(),[8])
        self.assertAlmostEqual(x[0,0],np.log(12),places=9)

    def test_empty_input_has_no_candidates(self):
        candidates,available,x,_=fusion_features(np.zeros(0),48000)
        self.assertEqual(len(candidates),0)
        self.assertEqual(len(available),0)
        self.assertEqual(x.shape,(0,9))

    def test_heldout_features_and_labels_cannot_change_fit(self):
        records=[record('a'),record('b',.1),record('held',10)]
        first=fit_without(records,'held')
        records[-1]['features']*=1000
        records[-1]['labels']=1-records[-1]['labels']
        self.assertEqual(first,fit_without(records,'held'))
        self.assertEqual(first['training_tracks'],['a','b'])

    def test_known_separable_features_produce_ordered_scores(self):
        model=fit_without([record('a'),record('b')],'unused')
        scores=predict_score(model,np.array([[-2.],[2.]]))
        self.assertLess(scores[0],.5)
        self.assertGreater(scores[1],.5)

    def test_refractory_uses_available_time_and_never_backdates(self):
        self.assertEqual(select_fires([.7,.9,.4,.8],np.array([8,10,13,15]),1000,10),[8,15])


if __name__=='__main__':unittest.main()
