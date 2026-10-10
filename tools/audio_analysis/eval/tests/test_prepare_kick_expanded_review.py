import unittest

from eval.prepare_kick_expanded_review import onset_to_master, select_cores


class ExpandedReviewTests(unittest.TestCase):
    def test_fixed_stratum_centres_and_disjoint_cores(self):
        rows = select_cores(140., [])
        self.assertEqual([r['start_s'] for r in rows], [14., 34., 54., 74., 94., 114.])
        self.assertTrue(all(r['end_s'] - r['start_s'] == 12 for r in rows))
        self.assertTrue(all(a['review_end_s'] < b['review_start_s'] for a, b in zip(rows, rows[1:])))

    def test_existing_core_and_margin_skip_without_replacement(self):
        rows = select_cores(140., [dict(id='old', start_s=46.4, end_s=50.)])
        self.assertEqual(rows[1]['status'], 'skipped_existing_review_overlap')
        self.assertEqual(rows[1]['start_s'], 34.)
        self.assertEqual(rows[1]['overlapping_existing_core_ids'], ['old'])
        self.assertEqual(sum(r['status'] == 'selected' for r in rows), 5)

    def test_source_offset_maps_timestamps_not_detector_latency(self):
        row = onset_to_master(116., .136375)
        self.assertAlmostEqual(row['master_proposal_s'], 116.136375)
        self.assertAlmostEqual(row['source_rms_support_s'][0], 115.998)
        self.assertAlmostEqual(row['master_mapped_rms_support_s'][0], 116.134375)
        self.assertIsNone(row['uncertainty']['master_onset_interval_s'])
        self.assertEqual(onset_to_master(42., 0.)['master_proposal_s'], 42.)


if __name__ == '__main__':
    unittest.main()
