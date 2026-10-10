"""Compact temporal relationships from the frozen eight-hop band trajectories.

One predeclared representation: retain the original fifteen features and replace
the twenty-four trajectory samples with nine shape descriptors. Band-normalised
power moments preserve timing direction and joint evolution. They do not add
observations, change candidates, or identify sources on their own.
"""
from __future__ import annotations

import numpy as np

from .kick_fusion_bandwise import FEATURE_NAMES as BASE_NAMES


PAIRS = ((0, 1), (0, 2), (1, 2))
FEATURE_NAMES = BASE_NAMES + (
    'low_after_body', 'low_after_upper', 'body_after_upper',
    'low_body_shape_agreement', 'low_upper_shape_agreement', 'body_upper_shape_agreement',
    'low_temporal_concentration', 'body_temporal_concentration', 'upper_temporal_concentration',
)


def shape_rows(trajectories):
    """Return signed relative time centres, concordance, and concentration.

Input shape is (candidates, three bands, eight ordered log-power observations).
Softmax over time gives each band's relative power distribution. Removing its
uniform component yields a centred shape; pair concordance is twice the inner
product divided by the two squared norms. A flat band contributes zero agreement.
Concentration is zero for uniform energy and approaches one for a single pulse.
Time differences use the observed seven-hop span, not an invented extra frame.

These shapes are invariant to a constant offset within each input trajectory.
The upstream clipping can already have lost information; it is not undone here.
"""
    values = np.asarray(trajectories, dtype=np.float64)
    if values.ndim != 3 or values.shape[1:] != (3, 8) or not np.all(np.isfinite(values)):
        raise ValueError('expected finite (candidates, 3, 8) trajectories')
    power = np.exp(values - np.max(values, axis=2, keepdims=True))
    mass = power / np.sum(power, axis=2, keepdims=True)
    centres = mass @ np.linspace(0., 1., 8)
    centred = mass - 1. / 8
    energy = np.sum(centred * centred, axis=2)
    delays = np.column_stack([centres[:, a] - centres[:, b] for a, b in PAIRS])
    agreement = np.column_stack([
        2 * np.sum(centred[:, a] * centred[:, b], axis=1)
        / (energy[:, a] + energy[:, b] + 1e-12) for a, b in PAIRS
    ])
    concentration = np.clip(energy * 8. / 7., 0., 1.)
    return np.column_stack((delays, agreement, concentration))


def from_cached_features(features):
    """Transform exactly the frozen39 schema without changing its first15 values."""
    values = np.asarray(features, dtype=np.float64)
    if values.ndim != 2 or values.shape[1] != 39 or not np.all(np.isfinite(values)):
        raise ValueError('expected finite frozen39 feature rows')
    return np.column_stack((values[:, :15], shape_rows(values[:, 15:].reshape(-1, 3, 8))))
