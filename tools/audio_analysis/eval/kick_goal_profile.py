"""Low-to-high rise profile of a candidate: where in frequency the new energy lands, early and late.

The causal 32-band log spectrum (kick_goal_templates.band_spec, 30 Hz-8 kHz) is
paired into 16 bands. Per band: the rise over the mean of the three frames ending
two hops before the candidate, maxed over the early half and over the late half
of the frames from the candidate hop to its own evidence deadline (never past
it). In dB. A kick's new energy settles below about 80 Hz as its pitch drops;
toms settle higher, claps and impacts spread wide, bass notes add little click.
"""
from __future__ import annotations

import numpy as np

PAIRS = 16
PROFILE_NAMES = tuple(f'rise_{w}_{b}' for w in ('early', 'late') for b in range(PAIRS))


def rise_profile(spec, cand, avail):
    paired = 10 * np.log10(0.5 * (10 ** spec[:, 0::2] + 10 ** spec[:, 1::2]))
    out = np.zeros((len(cand), 2 * PAIRS))
    for n, (c, a) in enumerate(zip(cand, avail)):
        if c < 6 or a >= len(paired):
            continue
        base = paired[c - 5:c - 2].mean(axis=0)
        mid = c + max(1, (a - c + 1) // 2)
        out[n, :PAIRS] = paired[c:mid].max(axis=0) - base
        out[n, PAIRS:] = paired[mid:a + 1].max(axis=0) - base if a + 1 > mid else out[n, :PAIRS]
    return out
