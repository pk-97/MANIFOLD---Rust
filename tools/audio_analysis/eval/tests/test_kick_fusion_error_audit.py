import unittest

import numpy as np
from scipy.special import expit

from eval.kick_fusion_error_audit import (
    annotate_label_context, availability_hop, classify_events, contributions, scored_labels,
)


class KickFusionErrorAuditTests(unittest.TestCase):
    def test_label_matched_at_50ms_but_outside_wide_early_boundary_is_kept(self):
        passage = dict(accuracy_by_tolerance_ms={'50': dict(
            pairs=[dict(attack_s=1.0, available_s=.96)], missed_times_s=[2.0])},
            association_early_35_late_200_ms=dict(pairs=[]), excluded_label_times_s=[3.0])
        labels = scored_labels([passage])
        self.assertEqual(labels, [1.0, 2.0])
        events = [dict(candidate_s=.92, available_s=.96)]
        annotate_label_context(events, [passage], labels)
        self.assertEqual(events[0]['nearest_scored_label_s'], 1.0)
        self.assertAlmostEqual(events[0]['nearest_scored_label_available_delta_ms'], -40.0)
        self.assertEqual(events[0]['nearest_excluded_label_s'], 3.0)

    def test_availability_uses_hop_end(self):
        for sr, hop in ((48000, 256), (44100, 235)):
            for index in (0, 8, 23456):
                self.assertEqual(availability_hop((index + 1) * hop / sr, sr, hop), index)
            with self.assertRaises(ValueError):
                availability_hop(8.5 * hop / sr, sr, hop)

    def test_original_clip_exclusions_and_extras(self):
        passage = dict(id='clip', excluded=[dict(start_s=0, end_s=.1)],
            association_early_35_late_200_ms=dict(matched=1, unmatched_triggers=1,
                pairs=[dict(attack_s=.2, available_s=.3)]))
        events = classify_events([0, 2, 4], [passage], 100, 10)
        self.assertEqual([(e['available_hop'], e['classification']) for e in events],
                         [(2, 'matched'), (4, 'wide_extra')])

    def test_master_margin_fires_do_not_become_core_extras(self):
        passage = dict(id='master', association_early_35_late_200_ms=dict(
            matched=1, extra=1, extra_times_s=[.5], pairs=[dict(attack_s=.2, available_s=.3)]))
        events = classify_events([0, 2, 4, 8], [passage], 100, 10)
        self.assertEqual([e['available_hop'] for e in events], [2, 4])

    def test_rejects_misaligned_frozen_scores(self):
        passage = dict(id='master', association_early_35_late_200_ms=dict(
            matched=0, extra=1, extra_times_s=[.3], pairs=[]))
        with self.assertRaises(ValueError):
            classify_events([0], [passage], 100, 10)

    def test_contributions_reconstruct_clipped_linear_logit(self):
        model = dict(mean=[2., -1.], scale=[.5, 2.], weights=[.3, -.8], intercept=.2)
        features = np.array([[2., -1.], [102., -201.], [1.5, 3.]])
        terms, logits = contributions(model, features)
        expected = np.array([[0., 0.], [2.4, 6.4], [-.3, -1.6]])
        np.testing.assert_allclose(terms, expected)
        np.testing.assert_allclose(logits, expected.sum(axis=1) + .2)
        self.assertTrue(np.all((expit(logits) > 0) & (expit(logits) < 1)))


if __name__ == '__main__':
    unittest.main()
