import copy
import unittest

import numpy as np

from eval.run_kick_dsp_experiments import evaluate
from eval.run_kick_shape_trial import append_reviews


def passage(name, start, truth):
    return dict(id=name, start_s=start, end_s=start+1., review_start_s=start-.25,
                review_end_s=start+1.25, kick_times_s=truth, uncertain_regions=[])


def record():
    source = dict(track='miracle', group='master', audio_sha256='audio',
                  passages=[passage('old', 1., [1.3])])
    return dict(track='miracle', source=source,
                ref=dict(variants=dict(v5=[133, 433]), scores=dict(v5=evaluate(source, [1.34, 4.34]))),
                sample_rate=1000, hop=10, candidates=np.array([129, 429, 449]),
                available=np.array([133, 433, 453]), features=np.zeros((3, 39)),
                cache_metadata=dict(duration_s=10.))


def review():
    core = dict(passage('new', 4., [4.3]), scoring_ready=True)
    return dict(status='lead_reviewed_visual_provisional',
                tracks=[dict(track='miracle', audio_sha256='audio', cores=[core])])


class ExpandedShapeRunnerTests(unittest.TestCase):
    def test_merge_preserves_old_inputs_and_uses_full_song_baseline(self):
        original = record()
        before = copy.deepcopy(original['source'])
        result, coverage = append_reviews([original], review())
        row = result[0]
        self.assertEqual(original['source'], before)
        self.assertEqual(row['original_passage_ids'], {'old'})
        self.assertIs(row['features'], original['features'])
        self.assertIs(row['available'], original['available'])
        self.assertEqual(row['labels'].tolist(), [1, 1, 0])
        self.assertTrue(row['training_mask'].all())
        self.assertEqual(row['ref']['scores']['v5'][0], original['ref']['scores']['v5'][0])
        self.assertEqual(row['ref']['scores']['v5'][1]['accuracy_by_tolerance_ms']['70']['matched'], 1)
        self.assertEqual(len(coverage), 1)

    def test_unscorable_core_stays_in_coverage_without_becoming_negative(self):
        expanded = review()
        expanded['tracks'][0]['cores'].append(dict(passage('uncertain', 6., []),
                                                  scoring_ready=False, reason='identity unresolved'))
        result, coverage = append_reviews([record()], expanded)
        self.assertEqual(len(result[0]['source']['passages']), 2)
        self.assertEqual(len(coverage), 2)
        self.assertFalse(coverage[-1]['scoring_ready'])
        expanded['tracks'][0]['cores'][0]['scoring_ready'] = False
        expanded['tracks'][0]['cores'][0]['reason'] = 'unresolved'
        with self.assertRaisesRegex(ValueError, 'no usable'):
            append_reviews([record()], expanded)

    def test_original_baseline_replay_mismatch_is_rejected(self):
        original = record()
        original['ref']['variants']['v5'] = [433]
        with self.assertRaisesRegex(ValueError, 'original prototype scoring changed'):
            append_reviews([original], review())

    def test_overlap_source_mismatch_and_unreviewed_input_are_rejected(self):
        for failure in ('overlap', 'hash', 'unreviewed', 'reserve'):
            expanded = review()
            if failure == 'overlap':
                expanded['tracks'][0]['cores'][0] = dict(passage('new', 1.1, [1.3]), scoring_ready=True)
            elif failure == 'hash':
                expanded['tracks'][0]['audio_sha256'] = 'different'
            elif failure == 'unreviewed':
                expanded['status'] = 'proposals'
            else:
                expanded['tracks'][0]['track'] = 'waypoints'
            with self.subTest(failure=failure), self.assertRaises(ValueError):
                append_reviews([record()], expanded)


if __name__ == '__main__':
    unittest.main()
