import unittest
import numpy as np
from eval.kick_failure_casebook import measurements


class CasebookMeasurementsTests(unittest.TestCase):
    def test_past_only_and_gain_invariant(self):
        signal=np.random.default_rng(42).normal(size=6000)
        expected=measurements(signal,48000,4000,960)
        self.assertEqual(expected,measurements(signal[:4000],48000,4000,960))
        scaled=measurements(signal*.25,48000,4000,960)
        np.testing.assert_allclose(list(expected.values()),list(scaled.values()),atol=1e-12)

    def test_identical_tone_windows_do_not_invent_change(self):
        signal=np.sin(2*np.pi*100*np.arange(6000)/48000)
        result=measurements(signal,48000,4800,960)
        self.assertAlmostEqual(result['growth_db'],0,places=10)
        self.assertAlmostEqual(result['spectral_change'],0,places=10)
        self.assertAlmostEqual(result['crest'],np.sqrt(2),places=10)

    def test_incomplete_or_silent_windows_are_explicit(self):
        with self.assertRaises(ValueError):measurements(np.zeros(4000),48000,1000,960)
        with self.assertRaises(ValueError):measurements(np.zeros(4000),48000,3000,960)
