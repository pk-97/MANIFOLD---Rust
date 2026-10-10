import contextlib
import io
import unittest

import numpy as np

from eval.kick_subspace_score import fit_without, predict_score
from eval.run_kick_dsp_experiments import evaluate
from eval.run_kick_trajectory_trial import negative_minutes, run_variant


class TrajectoryRunnerTests(unittest.TestCase):
    def test_subspace_flows_through_nested_refinement_and_original_clip_scoring(self):
        records = []
        for i in range(3):
            source = dict(track=str(i), group='original_five', truth=[.51, .71], regions=[])
            features = np.random.default_rng(i).normal(0, .1, (8, 6))
            features[4:, 0] += 2
            records.append(dict(track=str(i), source=source, features=features,
                labels=np.array([0]*4+[1]*4), training_mask=np.ones(8, bool),
                event_ids=np.array([-1]*4+[0, 0, 1, 1]), available=np.arange(8)*10,
                sample_rate=1000, hop=10, ref=dict(scores=dict(v5=evaluate(source, [.51, .71]))),
                cache_metadata={}))
        with contextlib.redirect_stdout(io.StringIO()):
            result = run_variant(records, 'test', fit_without, predict_score)
        self.assertEqual(result['totals']['baseline']['labels'], 6)
        self.assertEqual(result['kick_free_minutes'], 0)
        self.assertIsNone(result['kick_free_false_triggers_per_minute'])
        for row in result['tracks']:
            self.assertNotIn(row['track'], row['refinement']['threshold_selection_tracks'])
            self.assertEqual(set(row['scores']), {'baseline', 'fixed_05', 'calibrated', 'refined'})
            self.assertIn('p95_associated_delay_ms', row['diagnostics'])

    def test_kick_free_duration_merges_uncertain_intervals(self):
        source = dict(group='master', passages=[dict(start_s=0, end_s=60, kick_times_s=[],
            uncertain_regions=[dict(start_s=10, end_s=20), dict(start_s=15, end_s=30)])])
        self.assertAlmostEqual(negative_minutes(source), (60-(30.2-9.93))/60)
        source['passages'][0]['kick_times_s'] = [2]
        self.assertEqual(negative_minutes(source), 0)


if __name__ == '__main__': unittest.main()
