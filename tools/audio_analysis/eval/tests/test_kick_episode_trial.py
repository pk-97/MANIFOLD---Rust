import unittest
import numpy as np
from eval.kick_episode_trial import episode_eligibility, detect_episode


def env(rows):
    return np.array([[[low, 1.], [body, 1.], [.1, .1]] for low, body in rows])


class EpisodeTests(unittest.TestCase):
    def test_separated_balance_and_growth_within_attack(self):
        signal = env([(.1, 3), (10, 3)])
        self.assertEqual(episode_eligibility(signal, 48000, 256).tolist(), [False, True])
        self.assertEqual(detect_episode(signal, 48000, 256), [1])

    def test_rearm_dip_does_not_join_different_attacks(self):
        signal = env([(.1, 3), (.1, 1), (10, 3)])
        self.assertFalse(episode_eligibility(signal, 48000, 256).any())

    def test_episode_expires_without_reusing_old_balance(self):
        signal = env([(.1, 3), (.1, 1.5), (.1, 1.5), (.1, 1.5), (10, 3)])
        self.assertFalse(episode_eligibility(signal, 1000, 10).any())

    def test_original_eligibility_and_past_only_availability(self):
        signal = env([(3, 3), (3, 3), (.1, 1), (.1, 3), (10, 3)])
        full = episode_eligibility(signal, 48000, 256)
        self.assertTrue(full[0])
        for end in range(1, len(signal)+1):
            self.assertEqual(episode_eligibility(signal[:end], 48000, 256).tolist(), full[:end].tolist())

if __name__ == '__main__': unittest.main()
