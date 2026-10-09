"""Boundary and uncertain-reference accounting for full-context master scoring."""
import unittest
from eval.master_kick_comparison import score_passage

class MasterKickComparisonTests(unittest.TestCase):
    def passage(self, truth, regions=None):
        return dict(start_s=1.,end_s=2.,review_start_s=.75,review_end_s=2.25,
                    kick_times_s=truth,uncertain_regions=regions or [])

    def test_context_kick_does_not_create_extra_at_passage_start(self):
        result=score_passage([1.01,1.52],self.passage([.99,1.5]))
        self.assertEqual({k:result['accuracy_by_tolerance_ms']['50'][k] for k in ('matched','missed','extra')},
                         dict(matched=1,missed=0,extra=0))

    def test_late_response_outside_core_can_match_last_core_kick(self):
        result=score_passage([2.04],self.passage([1.99]))
        self.assertEqual(result['accuracy_by_tolerance_ms']['50']['matched'],1)

    def test_uncertain_audio_and_its_delayed_response_are_excluded(self):
        result=score_passage([1.15,1.52,1.8],self.passage([1.0,1.5],
                            [dict(start_s=.98,end_s=1.02,reason='masked')]))
        self.assertEqual(result['labels'],1)
        self.assertEqual(result['accuracy_by_tolerance_ms']['50']['extra'],1)

    def test_bass_only_passage_counts_every_fire(self):
        result=score_passage([1.2,1.4],self.passage([]))
        self.assertEqual(result['accuracy_by_tolerance_ms']['50']['extra'],2)
        self.assertEqual(result['labels'],0)

if __name__=='__main__':unittest.main()
