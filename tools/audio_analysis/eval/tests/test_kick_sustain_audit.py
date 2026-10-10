import unittest

import numpy as np

from eval.kick_sustain_audit import (
    body_persistence_mask, detect_body_persistence, validate_confirmed_fires,
)


def _env(rows):
    result = np.zeros((len(rows), 3, 2), dtype=float)
    for i, (fast, slow) in enumerate(rows):
        result[i, 1] = (fast, slow)
        result[i, 0] = (fast, slow)
        result[i, 2] = (0.1, 0.1)
    return result


class BodyPersistenceTests(unittest.TestCase):
    def test_rejects_single_frame_body_attack(self):
        env = _env([(3.0, 1.0), (0.5, 1.0)])
        self.assertEqual(body_persistence_mask(env).tolist(), [False, False])

    def test_two_frame_body_attack_is_available_on_second_completed_hop(self):
        env = _env([(3.0, 1.0), (3.0, 1.0), (0.5, 1.0)])
        self.assertEqual(body_persistence_mask(env).tolist(), [False, True, False])
        self.assertEqual(detect_body_persistence(env, 48_000, 256), [1])

    def test_mask_has_no_future_leak(self):
        prefix = _env([(0.5, 1.0), (3.0, 1.0)])
        future = _env([(0.5, 1.0), (3.0, 1.0), (3.0, 1.0)])
        self.assertEqual(body_persistence_mask(prefix).tolist(), [False, False])
        self.assertEqual(body_persistence_mask(future)[:2].tolist(), [False, False])


class ConfirmedPersistenceTests(unittest.TestCase):
    def test_two_stage_evidence_preserves_decision_and_uses_later_time(self):
        # Low confirmation exists at the original fire, but is gone when
        # persistence becomes available. Do not require it a second time.
        env = _env([(0.5, 1), (3, 1), (3, 1)])
        env[2, 0] = (.1, 1)
        fires, decisions = validate_confirmed_fires(env, [1], 48000, 256)
        self.assertEqual(fires, [2])
        self.assertEqual(decisions, [dict(original_hop=1, accepted_hop=2)])

    def test_existing_persistence_needs_no_wait(self):
        fires, _ = validate_confirmed_fires(_env([(3, 1), (3, 1), (.5, 1)]),
                                             [2], 48000, 256)
        self.assertEqual(fires, [2])

    def test_expired_attack_and_absent_original_fire_cannot_trigger(self):
        env = _env([(3, 1), (3, 1)] + [(.5, 1)] * 7)
        self.assertEqual(validate_confirmed_fires(env, [8], 48000, 256)[0], [])
        self.assertEqual(validate_confirmed_fires(env, [], 48000, 256)[0], [])

    def test_wait_budget_includes_six_hops_but_not_seven(self):
        inside = _env([(.5, 1)] * 5 + [(3, 1), (3, 1)])
        outside = _env([(.5, 1)] * 6 + [(3, 1), (3, 1)])
        self.assertEqual(validate_confirmed_fires(inside, [0], 48000, 256)[0], [6])
        self.assertEqual(validate_confirmed_fires(outside, [0], 48000, 256)[0], [])

    def test_no_future_output_and_one_output_per_original(self):
        env = _env([(.5, 1), (3, 1), (3, 1), (3, 1), (3, 1)])
        full = validate_confirmed_fires(env, [0], 48000, 256)[0]
        self.assertEqual(full, [2])
        for end in range(1, len(env) + 1):
            self.assertEqual(validate_confirmed_fires(env[:end], [0], 48000, 256)[0],
                             [i for i in full if i < end])


if __name__ == "__main__":
    unittest.main()
