import unittest
import numpy as np
from eval.kick_dsp_experiments import detect_v5

class KickDSPTests(unittest.TestCase):
    def test_sequential_evidence_recovers_event_without_backdating(self):
        # First frame has attack ratio but insufficient body/low balance;
        # second frame has balance but insufficient body attack ratio.
        env=np.array([[[3.,.5],[.9,.3],[.1,.1]],
                      [[2.,.4],[1.,.6],[.1,.1]]])
        self.assertEqual(detect_v5(env,48000,256),[])
        self.assertEqual(detect_v5(env,48000,256,eligibility_window_s=.020),[1])

    def test_expired_evidence_cannot_recover_event(self):
        env=np.array([[[3.,.5],[.9,.3],[.1,.1]],
                      [[.1,.1],[.1,.1],[.1,.1]],
                      [[2.,.4],[1.,.6],[.1,.1]]])
        self.assertEqual(detect_v5(env,48000,1024,eligibility_window_s=.020),[])

    def test_confirmation_waits_for_available_gate(self):
        env=np.array([[[1.,.1],[1.,.1],[.1,.1]]]*3)
        self.assertEqual(detect_v5(env,48000,256,confirmation_mask=[False,False,True]),[2])

    def test_future_cannot_change_prefix(self):
        rng=np.random.default_rng(41)
        env=rng.uniform(.01,1,(300,3,2))
        full=detect_v5(env,48000,256,eligibility_window_s=.020)
        self.assertEqual([i for i in full if i<150],
                         detect_v5(env[:150],48000,256,eligibility_window_s=.020))

    def test_silence_stays_silent(self):
        self.assertEqual(detect_v5(np.zeros((300,3,2)),48000,256,eligibility_window_s=.020),[])

if __name__=='__main__':unittest.main()
