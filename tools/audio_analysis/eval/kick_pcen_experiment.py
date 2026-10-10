"""Causal PCEN candidate-mask experiment for the fixed v5 kick detector.

The input envelopes are the existing native-rate ``(hop, band, fast/slow)``
power envelopes.  PCEN is used only to replace v5's candidate and rearm
decisions; confirmation, peak adaptation, timeout, and refractory behaviour
remain in :func:`eval.kick_dsp_experiments.detect_v5`.

This is a first probe with fixed constants.  It is intentionally CPU-only and
does not score audio or tune the constants against a fixture set.  PCEN is
approximately level invariant once the EMA is established; the finite
``1e-6`` epsilon makes exact scale invariance impossible near silence.
"""
from __future__ import annotations

import math

import numpy as np

from eval.kick_dsp_experiments import detect_v5


BACKGROUND_TAU_S = 0.4
PCEN_EPSILON = 1e-6
PCEN_GAIN = 0.98
PCEN_OFFSET = 2.0
NOVELTY_LOOKBACK_HOPS = 2
NOVELTY_CANDIDATE_THRESHOLD = 0.10
NOVELTY_REARM_THRESHOLD = 0.05


def _validated_envelopes(envelopes: np.ndarray) -> np.ndarray:
    """Return finite, non-negative float64 power envelopes without mutation."""
    env = np.asarray(envelopes, dtype=np.float64)
    if env.ndim != 3 or env.shape[1:] != (3, 2):
        raise ValueError("expected (hop, band, fast/slow) envelopes")
    if not np.all(np.isfinite(env)):
        raise ValueError("envelopes must contain only finite values")
    if np.any(env < 0.0):
        raise ValueError("power envelopes must be non-negative")
    return env


def pcen_features(envelopes, sample_rate, hop):
    """Return the PCEN arrays and masks used by :func:`detect`.

    The returned mapping contains ``pcen`` (all three bands), ``novelty``
    (the positive two-hop low-plus-body difference), ``eligible_mask`` and
    ``rearm_mask``.  The EMA starts at zero for every call, so every value is
    causal and prefix-stable.
    """
    if sample_rate <= 0 or hop <= 0:
        raise ValueError("sample_rate and hop must be positive")
    env = _validated_envelopes(envelopes)
    fast = env[:, :, 0]
    count = len(env)

    # The recurrence is evaluated once per completed hop.  Keeping the EMA in
    # a scalar loop makes the zero-initialized causal state explicit and avoids
    # accidentally using a non-causal vectorized filter.
    alpha = math.exp(-hop / (sample_rate * BACKGROUND_TAU_S))
    background = np.zeros_like(fast)
    state = np.zeros(3, dtype=np.float64)
    for index, energy in enumerate(fast):
        state = alpha * state + (1.0 - alpha) * energy
        background[index] = state

    pcen = np.sqrt(
        fast / np.power(PCEN_EPSILON + background, PCEN_GAIN) + PCEN_OFFSET
    ) - math.sqrt(PCEN_OFFSET)

    novelty = np.zeros(count, dtype=np.float64)
    if count > NOVELTY_LOOKBACK_HOPS:
        difference = pcen[NOVELTY_LOOKBACK_HOPS:] - pcen[:-NOVELTY_LOOKBACK_HOPS]
        novelty[NOVELTY_LOOKBACK_HOPS:] = np.maximum(difference[:, 0], 0.0)
        novelty[NOVELTY_LOOKBACK_HOPS:] += np.maximum(difference[:, 1], 0.0)

    original_power = fast[:, 0] + fast[:, 1]
    eligible_mask = (
        (novelty >= NOVELTY_CANDIDATE_THRESHOLD)
        & (original_power > 1e-6)
        & (fast[:, 1] > fast[:, 0] / 3.0)
    )
    rearm_mask = novelty < NOVELTY_REARM_THRESHOLD
    return {
        "pcen": pcen,
        "background": background,
        "novelty": novelty,
        "eligible_mask": eligible_mask,
        "rearm_mask": rearm_mask,
    }


def detect(samples, sample_rate, envelopes, hop):
    """Return zero-based fire-hop indices for the causal PCEN experiment.

    ``samples`` is accepted to keep this detector interchangeable with the
    evaluation harness.  The experiment deliberately consumes the supplied
    envelopes, so it does not recompute or inspect audio samples.
    """
    del samples
    features = pcen_features(envelopes, sample_rate, hop)
    return detect_v5(
        envelopes,
        sample_rate,
        hop,
        eligible_mask=features["eligible_mask"],
        rearm_mask=features["rearm_mask"],
    )
