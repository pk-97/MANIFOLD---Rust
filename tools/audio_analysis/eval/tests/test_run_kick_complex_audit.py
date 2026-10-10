import unittest
import numpy as np
from eval.run_kick_complex_audit import auc, fire_evidence, window_peak


class ComplexAuditTests(unittest.TestCase):
    def test_window_keeps_available_endpoint_and_excludes_future(self):
        grid=np.arange(1,11)*.005
        values=np.array([0,0,0,.3,.1,9,9,9,9,9])
        self.assertEqual(window_peak(values,grid,.010,.025),
                         dict(value=.3,available_s=.020))
        self.assertEqual(fire_evidence(values,grid,.025)['recent_peak']['value'],.3)
        with self.assertRaises(ValueError):fire_evidence(values,grid,.024)

    def test_old_evidence_expires(self):
        grid=np.arange(1,11)*.005
        values=np.array([9,9,9,9,9,0,0,0,0,.1])
        self.assertEqual(fire_evidence(values,grid,.05)['recent_peak']['value'],.1)

    def test_pairwise_ranking_counts_ties(self):
        self.assertEqual(auc([2,3],[0,1]),1)
        self.assertEqual(auc([0,1],[2,3]),0)
        self.assertEqual(auc([1],[1]),.5)
        self.assertIsNone(auc([], [1]))
