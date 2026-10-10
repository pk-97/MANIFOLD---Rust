import unittest

import numpy as np

from eval.kick_attack_rejection import _classification, causal_features, ratio_growth_gate


class KickAttackRejectionTests(unittest.TestCase):
    def setUp(self):
        self.sr = 48_000
        self.hop = 256

    def _envelopes(self, values):
        env = np.zeros((len(values), 3, 2), dtype=float)
        for index, (low, body) in enumerate(values):
            env[index, 0, 0] = low
            env[index, 1, 0] = body
        return env

    def test_gate_reports_confirmation_hop_end(self):
        env = self._envelopes([(1, 1), (4, 1), (5, 1)])
        accepted, decisions = ratio_growth_gate(env, [0], self.hop, self.sr)
        self.assertEqual(accepted, [2 * self.hop / self.sr])
        self.assertEqual(decisions[0]["status"], "accepted")
        self.assertEqual(decisions[0]["confirmation_hop"], 1)

    def test_gate_rejects_growth_beyond_timeout(self):
        count = 10
        env = self._envelopes([(1, 1)] + [(1, 1)] * (count - 2) + [(4, 1)])
        accepted, decisions = ratio_growth_gate(env, [0], self.hop, self.sr)
        self.assertEqual(accepted, [])
        self.assertEqual(decisions[0]["status"], "rejected_timeout")

    def test_gate_is_prefix_causal(self):
        prefix = self._envelopes([(1, 1)] * 2)
        suffix = self._envelopes([(4, 1), (1, 1)])
        accepted, _ = ratio_growth_gate(prefix, [0], self.hop, self.sr)
        accepted_with_suffix, _ = ratio_growth_gate(
            np.concatenate((prefix, suffix)), [0], self.hop, self.sr
        )
        prefix_end = len(prefix) * self.hop / self.sr
        self.assertEqual(accepted, [t for t in accepted_with_suffix if t <= prefix_end])
        self.assertEqual(accepted_with_suffix, [3 * self.hop / self.sr])

    def test_silence_and_stationary_ratio_do_not_fire(self):
        silence = self._envelopes([(0, 0)] * 10)
        stationary = self._envelopes([(1, 1)] * 10)
        self.assertEqual(ratio_growth_gate(silence, [0], self.hop, self.sr)[0], [])
        self.assertEqual(ratio_growth_gate(stationary, [0], self.hop, self.sr)[0], [])

    def test_features_are_native_rate_and_hop_aligned(self):
        features, hop = causal_features(np.zeros(self.sr), self.sr)
        self.assertEqual(hop, self.hop)
        self.assertEqual(features.shape, (self.sr // self.hop, 3, 2))
        self.assertTrue(np.all(features == 0.0))

    def test_uncertain_events_stay_out_of_extra_attribution(self):
        review = [{"review_status": "clip_boundary", "estimated_attack_s": "0"},
                  {"review_status": "needs_listening", "estimated_attack_s": "2"}]
        self.assertEqual(_classification([.1, 1.02, 1.5, 1.95], [1.0], review, 2.2),
                         ["excluded_clip_boundary", "matched_association", "scored_extra",
                          "excluded_unresolved_ending"])


if __name__ == "__main__":
    unittest.main()
