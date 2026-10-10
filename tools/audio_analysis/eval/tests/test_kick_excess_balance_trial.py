import unittest

import numpy as np

from eval.kick_dsp_experiments import detect_v5
from eval.kick_excess_balance_trial import detect_excess, excess_eligibility


class ExcessBalanceTests(unittest.TestCase):
    def test_background_can_mask_balance_without_masking_attack(self):
        # Body/low is 0.25; rising body/low is 0.4. Other v5 rules pass.
        env = np.array([[[8., 3.], [2., .0], [.1, .1]]])
        self.assertEqual(detect_v5(env, 48000, 256), [])
        self.assertEqual(detect_excess(env, 48000, 256), [0])

    def test_new_low_surge_can_reject_previously_eligible_balance(self):
        # This is a replacement rule, not a promise of baseline preservation.
        env = np.array([[[6., 0.], [3., 1.4], [.1, .1]]])
        self.assertEqual(detect_v5(env, 48000, 256), [0])
        self.assertEqual(detect_excess(env, 48000, 256), [])

    def test_floor_rise_and_low_confirmation_still_apply(self):
        for env in (np.zeros((3, 3, 2)),
                    np.array([[[1., 1.], [1., 1.], [.1, .1]]]),
                    np.array([[[.1, 1.], [4., 1.], [.1, .1]]])):
            self.assertEqual(detect_excess(env, 48000, 256), [])

    def test_future_samples_do_not_change_existing_decisions(self):
        env = np.random.default_rng(42).uniform(.01, 4, (40, 3, 2))
        mask = excess_eligibility(env)
        fires = detect_excess(env, 48000, 256)
        for end in range(1, len(env)+1):
            np.testing.assert_array_equal(excess_eligibility(env[:end]), mask[:end])
            self.assertEqual(detect_excess(env[:end], 48000, 256),
                             [i for i in fires if i < end])


if __name__ == '__main__':
    unittest.main()
