import unittest
from eval.run_kick_timbre_trial import completed_index


class TimbreDeadlineTests(unittest.TestCase):
    def test_deadline_uses_latest_completed_hop_without_waiting(self):
        index=completed_index(.050,48000,256,100)
        self.assertEqual(index,8)
        self.assertLessEqual((index+1)*256/48000,.050)
        for sr,hop in ((48000,256),(44100,235)):
            time=91*hop/sr
            self.assertEqual(completed_index(time,sr,hop,100),90)

    def test_incomplete_and_past_eof_are_explicit_errors(self):
        with self.assertRaises(ValueError):completed_index(.001,48000,256,100)
        with self.assertRaises(ValueError):completed_index(1.,48000,256,100)
