import unittest
import numpy as np
from eval.kick_hybrid_experiment import detect_from_features, detect_attack_led

class HybridTests(unittest.TestCase):
    def test_attack_led_requires_stable_low_confirmation(self):
        raw=np.array([[[1., 1.], [1., .1], [.1, .1]]]*3)
        stable=raw.copy()
        self.assertEqual(detect_attack_led(raw,stable,48000,256),[])
        self.assertEqual(detect_attack_led(raw,stable,48000,256,False),[0])
        stable[2,0,:]=[1.,.1]
        self.assertEqual(detect_attack_led(raw,stable,48000,256),[2])

    def test_silence_is_silent(self):
        env=np.zeros((100,3,2))
        self.assertEqual(detect_from_features(env,env,48000,256),[])

    def test_raw_attack_is_required_only_in_hybrid(self):
        raw=np.full((100,3,2),.01)
        stable=raw.copy();stable[20:24,:,0]=.5
        # Stable novelty and low confirmation exist, but no raw attack.
        self.assertEqual(detect_from_features(raw,stable,48000,256),[])
        self.assertTrue(detect_from_features(raw,stable,48000,256,False))

    def test_no_future_features_change_past_decisions(self):
        rng=np.random.default_rng(63)
        raw=rng.uniform(.001,1,(300,3,2));stable=rng.uniform(.001,1,(300,3,2))
        for hybrid in (False,True):
            full=detect_from_features(raw,stable,48000,256,hybrid)
            self.assertEqual([i for i in full if i<150],
                detect_from_features(raw[:150],stable[:150],48000,256,hybrid))
        for require_rise in (False,True):
            full=detect_attack_led(raw,stable,48000,256,require_rise)
            self.assertEqual([i for i in full if i<150],
                detect_attack_led(raw[:150],stable[:150],48000,256,require_rise))

if __name__=='__main__':unittest.main()
