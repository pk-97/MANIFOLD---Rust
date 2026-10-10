"""The frozen 15 kick features over a shorter causal evidence window.

The frozen modules hard-code a 40 ms evidence deadline. They are not edited
(their hashes key the evening caches); instead the deadline arithmetic is
scaled through a module-level shim while they run. Candidates are the same rise
edges; only the evidence deadline and therefore the availability hop change.
"""
from __future__ import annotations

import math
from unittest.mock import patch

from . import kick_fusion_bandwise as bandwise
from . import kick_fusion_features as base

FROZEN_EVIDENCE_S = .040


def fast_features(samples, sample_rate, evidence_s):
    """Return (candidates, available, features[:, :15], hop) with a shorter deadline."""
    if not 0 < evidence_s <= FROZEN_EVIDENCE_S:
        raise ValueError('evidence window must shorten the frozen 40 ms deadline')

    class Shim:
        floor = staticmethod(math.floor)

        @staticmethod
        def ceil(x):
            return math.ceil(x * evidence_s / FROZEN_EVIDENCE_S)

    with patch.object(base, 'math', Shim):
        return bandwise.fusion_features(samples, sample_rate)
