"""Boundary and exclusion checks for new-passage validation, with no audio access."""
import copy
import unittest

from .verify_kick_evening_passages import assert_holdout, score_reviewed_core


def opening():
    return dict(start_s=0,end_s=1,review_start_s=0,review_end_s=1.25,
        kick_times_s=[.1,1.02],uncertain_regions=[],
        raw_excerpt_coordinates=dict(master=dict(source_sample_range=[0,60000],master_axis_origin_s=0)))


class PassageTests(unittest.TestCase):
    def test_startup_fire_is_counted_and_right_margin_label_absorbs_response(self):
        passage=opening();before=copy.deepcopy(passage)
        result=score_reviewed_core([0,.14,.99],passage)
        score=result['accuracy_by_tolerance_ms']['70']
        self.assertEqual((score['matched'],score['missed'],score['extra']),(1,0,1))
        self.assertEqual(score['extra_times_s'],[0])
        self.assertEqual(passage,before)
        self.assertTrue(result['recording_start_boundary'])

    def test_uncertain_audio_excludes_its_late_response(self):
        passage=opening();passage['uncertain_regions']=[dict(start_s=.4,end_s=.45,reason='unknown')]
        score=score_reviewed_core([.14,.62,.8],passage)['accuracy_by_tolerance_ms']['70']
        self.assertEqual(score['extra_times_s'],[.8])

    def test_recording_boundary_cannot_hide_negative_time_or_missing_context(self):
        with self.assertRaises(ValueError):score_reviewed_core([-.01],opening())
        passage=opening();passage['raw_excerpt_coordinates']['master']['source_sample_range'][0]=1
        with self.assertRaises(ValueError):score_reviewed_core([],passage)
        passage=opening();passage.update(start_s=.1,review_start_s=0)
        with self.assertRaises(ValueError):score_reviewed_core([],passage)

    def test_nested_scorer_cannot_train_on_validation_family(self):
        with self.assertRaises(ValueError):
            assert_holdout(dict(training_tracks=['a'],component=dict(training_tracks=['b'])),'b')
        assert_holdout(dict(training_tracks=['a'],component=dict(training_tracks=['c'])),'b')


if __name__ == '__main__':
    unittest.main()
