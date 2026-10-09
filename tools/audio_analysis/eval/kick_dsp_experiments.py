"""Fixed CPU-only kick experiments; sample-clock decisions, no backdating.

Reference v5 reproduces the standalone Rust probe. Optional masks replace only
one specified decision component. Temporal eligibility retains independent
predicate evidence for 20 ms; it does not delay or rewrite recorded events.
"""
from __future__ import annotations

import math
import numpy as np

TEMPORAL_WINDOW_S = 0.020


def detect_v5(envelopes, sample_rate, hop, *, eligibility_window_s=0.0,
              eligible_mask=None, rearm_mask=None, confirmation_mask=None):
    if sample_rate <= 0 or hop <= 0 or eligibility_window_s < 0:
        raise ValueError('invalid sample grid or evidence window')
    env = np.asarray(envelopes)
    if env.ndim != 3 or env.shape[1:] != (3, 2):
        raise ValueError('expected (hop, band, fast/slow) envelopes')
    for mask in (eligible_mask, rearm_mask, confirmation_mask):
        if mask is not None and np.shape(mask) != (len(env),):
            raise ValueError('mask must have one entry per completed hop')
    if eligibility_window_s and eligible_mask is not None:
        raise ValueError('experiments must not combine candidate replacements')
    dt = hop / sample_rate
    peak, last_fire, armed, pending = 0.0, -1.0, True, None
    last_evidence = [-math.inf] * 4
    fires = []
    for i, bands in enumerate(env):
        time = (i + 1) * dt
        low, body, mid = bands
        power = low[0] + body[0]
        ratio = body[0] / (body[1] + 1e-12)
        peak = max(low[0], peak * math.exp(-dt / 2.0))
        if (ratio < 1.2) if rearm_mask is None else bool(rearm_mask[i]):
            armed = True
        predicates = (power > 1e-6, ratio > 2.0,
                      power > 1.8 * (low[1] + body[1]), body[0] > low[0] / 3.0)
        if eligible_mask is not None:
            eligible = bool(eligible_mask[i])
        elif eligibility_window_s:
            for j, passed in enumerate(predicates):
                if passed:
                    last_evidence[j] = time
            # Keep the absolute floor current: old evidence must not fire in silence.
            eligible = predicates[0] and all(time - t <= eligibility_window_s
                                              for t in last_evidence)
        else:
            eligible = all(predicates)
        if pending is None and armed and eligible and time - last_fire >= .060:
            pending = time
            armed = False
            # Evidence cannot seed another candidate after being consumed.
            last_evidence = [-math.inf] * 4
        if pending is not None:
            confirmation = (low[0] > max(1e-6, peak * .15)
                            and low[0] > low[1] * 1.2 and power > mid[0] * .8)
            if confirmation_mask is not None:
                confirmation = confirmation and bool(confirmation_mask[i])
            if confirmation:
                fires.append(i)
                last_fire, pending = time, None
            elif time - pending >= .035:
                pending = None
    return fires


def detect_temporal(samples, sample_rate, envelopes, hop):
    return detect_v5(envelopes, sample_rate, hop,
                     eligibility_window_s=TEMPORAL_WINDOW_S)
