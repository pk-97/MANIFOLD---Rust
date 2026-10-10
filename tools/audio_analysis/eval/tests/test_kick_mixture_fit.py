import unittest

import numpy as np
from scipy.optimize import nnls

from eval.kick_mixture_fit import (
    causal_spectra,
    detect_activation,
    explained_kick_power,
    nnls_improvement,
)


class KickMixtureFitTests(unittest.TestCase):
    def test_fit_improvement_scales_with_input_power(self):
        prior = np.array([.8, .1, .4])
        template = np.array([.1, .9, .2])
        spectra = np.array([prior + template])
        expected = nnls_improvement(spectra, prior, template)
        for scale in (1e-9, 1e-3, 10):
            actual = nnls_improvement(spectra*scale, prior*scale, template)
            np.testing.assert_allclose(actual, expected*scale**2, rtol=1e-10, atol=0)

    def test_nnls_matches_scipy_reference(self):
        rng = np.random.default_rng(12)
        spectra = rng.random((24, 9))
        background = rng.random((24, 9))
        template = rng.random(9)
        actual = nnls_improvement(spectra, background, template)
        template = template / np.linalg.norm(template)
        expected = []
        for signal, prior in zip(spectra, background):
            errors = [
                np.sum((signal - prior * nnls(prior[:, None], signal)[0][0]) ** 2),
                np.sum((signal - template * nnls(template[:, None], signal)[0][0]) ** 2),
            ]
            coefficients, _ = nnls(np.column_stack((prior, template)), signal)
            errors.append(np.sum((signal - np.column_stack((prior, template)) @ coefficients) ** 2))
            expected.append(errors[0] - min(errors))
        np.testing.assert_allclose(actual, expected, rtol=1e-10, atol=1e-12)

    def test_known_kick_and_background_have_explained_power(self):
        rng = np.random.default_rng(8)
        template = np.zeros(12); template[4:7] = [0.4, 1.0, 0.6]
        prior = np.linspace(0.1, 0.4, 12)
        spectra = np.tile(prior, (30, 1))
        spectra[15] += 0.8 * template
        power = explained_kick_power(spectra, template, 48_000, 256)
        self.assertGreater(power[15], 0.1)
        self.assertLess(power[10], 1e-12)

    def test_collinear_and_silent_background_cases(self):
        template = np.array([0.2, 0.8, 0.4])
        collinear = np.tile(template, (3, 1))
        np.testing.assert_allclose(nnls_improvement(collinear, collinear, template), 0.0, atol=1e-12)
        signal = np.tile(template, (3, 1))
        silent = np.zeros_like(signal)
        np.testing.assert_allclose(nnls_improvement(signal, silent, template), np.sum(signal * signal, axis=1), atol=1e-12)

    def test_spectra_and_power_are_prefix_invariant(self):
        rng = np.random.default_rng(4)
        prefix = rng.normal(size=4096)
        suffix = rng.normal(size=4096)
        short = causal_spectra(prefix, 48_000, 256)
        full = causal_spectra(np.concatenate((prefix, suffix)), 48_000, 256)
        np.testing.assert_array_equal(short, full[:len(short)])
        template = np.linspace(0.1, 1.0, short.shape[1])
        np.testing.assert_allclose(
            explained_kick_power(short, template, 48_000, 256),
            explained_kick_power(full, template, 48_000, 256)[:len(short)],
            atol=1e-12,
        )

    def test_activation_is_causal_and_silence_is_quiet(self):
        rng = np.random.default_rng(3)
        prefix = np.zeros(500)
        prefix[100:108] = 1.0
        prefix[300:308] = 1.0
        suffix = rng.random(200)
        short = detect_activation(prefix, 48_000, 256)
        full = detect_activation(np.concatenate((prefix, suffix)), 48_000, 256)
        self.assertEqual(short, full[:len(short)])
        self.assertGreaterEqual(len(short), 2)
        self.assertEqual(detect_activation(np.zeros(200), 48_000, 256), [])


if __name__ == "__main__":
    unittest.main()
