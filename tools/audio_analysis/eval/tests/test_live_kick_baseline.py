"""Scoring checks: duplicate accounting, uncertainty, and causal availability."""
import unittest

from eval.live_kick_baseline import exclusion_regions, match_events, parse_harness, score_events


class LiveKickBaselineTests(unittest.TestCase):
    def test_maximum_one_to_one_assignment(self):
        # Nearest-first would consume 1.05 for 1.04 and miss the second hit.
        self.assertEqual(match_events([1.04, 1.08], [1.0, 1.05], .05, .05),
                         ((0, 0), (1, 1)))

    def test_duplicate_is_extra_not_a_second_recovered_hit(self):
        score = score_events([1.02, 1.06], [1.0], [])
        tight = score["accuracy_by_tolerance_ms"]["70"]
        self.assertEqual((tight["matched"], tight["missed"], tight["extra"]), (1, 0, 1))
        self.assertEqual(score["association_early_35_late_200_ms"]["possible_duplicate_times_s"], [1.06])

    def test_late_association_does_not_improve_headline_accuracy(self):
        score = score_events([1.12], [1.0], [])
        tight = score["accuracy_by_tolerance_ms"]["50"]
        self.assertEqual((tight["matched"], tight["missed"], tight["extra"]), (0, 1, 1))
        self.assertEqual(score["association_early_35_late_200_ms"]["median_ms"], 120.0)

    def test_uncertain_and_boundary_events_are_not_negative_examples(self):
        review = [{"review_status": "clip_boundary", "estimated_attack_s": "0.000"},
                  {"review_status": "needs_listening", "estimated_attack_s": "2.000"}]
        regions = exclusion_regions(review, 2.2)
        score = score_events([.1, 1.02, 1.95, 2.18], [1.0], regions)
        self.assertEqual(score["scored_triggers"], 1)
        self.assertEqual([r["trigger_times_s"] for r in score["excluded"]], [[.1], [1.95, 2.18]])
        self.assertEqual(score["accuracy_by_tolerance_ms"]["50"]["extra"], 0)
        with self.assertRaises(ValueError):
            score_events([.1], [.05], regions)

    def test_availability_is_end_of_hop_not_start(self):
        sr, hop, count, indices, times = parse_harness(
            "clip: 1.00s @ 48000 Hz, 187 hops of 256 samples (5.33 ms)\n"
            "P3 clip: kick_hops=[0, 17]\n")
        self.assertEqual((sr, hop, count, indices), (48000, 256, 187, [0, 17]))
        self.assertAlmostEqual(times[0], 256 / 48000)
        self.assertAlmostEqual(times[1], 18 * 256 / 48000)
        with self.assertRaises(ValueError):
            parse_harness("no event log")

    def test_empty_predictions_keep_misses(self):
        score = score_events([], [1.0, 2.0], [])
        self.assertEqual(score["accuracy_by_tolerance_ms"]["50"]["missed"], 2)
        self.assertIsNone(score["association_early_35_late_200_ms"]["median_ms"])


if __name__ == "__main__":
    unittest.main()
